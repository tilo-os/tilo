//! What the compositor draws hosted surfaces' frames with.
//!
//! A frame a surface drew on the GPU is shown from the dma-buf it drew into, imported as any
//! client's is: nothing passes through memory. A frame drawn on the CPU (a machine with no GPU to
//! draw on) has one memory buffer, holding the pixels that frame showed when it was last drawn,
//! brought up to date with just the parts the surface changed since.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::texture::{TextureRenderBuffer, TextureRenderElement};
use smithay::backend::renderer::{ContextId, ImportAll, ImportDma, ImportMem, Renderer};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

use tilo_session_core::surface::{HostBuffers, HostFrame, Region};

use crate::render::OutputRenderElement;

/// The one format hosted surfaces draw their GPU frames in: R, G, B, A bytes in memory, as their
/// renderer draws, in a linear layout every GPU that imports dma-bufs reads.
const FRAME_FORMAT: Fourcc = Fourcc::Abgr8888;

/// The render node hosted surfaces allocate the buffers of their GPU frames on: the compositor's,
/// once it imports their format there.
static FRAMES_NODE: OnceLock<PathBuf> = OnceLock::new();

/// Hosted surfaces may draw their frames on the GPU into buffers on `node`, when the renderer
/// imports their format (`formats`). Called once, by the backend's renderer as it starts.
pub(crate) fn offer_gpu_frames(node: Option<&Path>, formats: &[Format]) {
    let imports = formats
        .iter()
        .any(|format| format.code == FRAME_FORMAT && format.modifier == Modifier::Linear);
    if let Some(node) = node.filter(|_| imports) {
        let _ = FRAMES_NODE.set(node.to_path_buf());
    }
}

/// The render node a hosted surface allocates its GPU frames' buffers on, as `/dev/dri/renderD128`:
/// `None` where the compositor imports none (no GPU, or none that reads them), and the surface
/// draws on the CPU.
#[must_use]
pub fn gpu_frames_node() -> Option<&'static Path> {
    FRAMES_NODE.get().map(PathBuf::as_path)
}

/// The buffers the hosted surfaces' frames are drawn from, by the frame they hold.
#[derive(Debug, Default)]
pub(crate) struct Uploads {
    frames: RefCell<HashMap<usize, Drawn>>,
}

/// One frame as this compositor draws it.
#[derive(Debug)]
struct Drawn {
    /// The frame's pixels, watched so a frame the surface dropped takes its buffer with it (a
    /// surface that resized, a host that went).
    frame: tilo_session_core::surface::FrameWatch,
    shown: Shown,
}

#[derive(Debug)]
enum Shown {
    Memory(MemoryRenderBuffer),
    Gpu(OnGpu),
}

/// A frame drawn on the GPU: its dma-bufs, and the texture the renderer that draws it shows.
struct OnGpu {
    dmabufs: Vec<Dmabuf>,
    /// The buffer the frame shows now.
    shown: Option<usize>,
    /// What changed since the renderer last drew the frame, and its opaque parts.
    pending: Vec<Rectangle<i32, Buffer>>,
    opaque: Option<Vec<Rectangle<i32, Buffer>>>,
    /// The renderer's `TextureRenderBuffer` of the buffer it drew last, with the renderer's
    /// context: one of its texture type.
    texture: Option<Box<dyn Any>>,
}

impl std::fmt::Debug for OnGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnGpu")
            .field("buffers", &self.dmabufs.len())
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

/// Where a hosted layer is drawn: at `at` (physical), the part `src` of its frame scaled to `size`.
#[derive(Clone, Copy)]
pub(crate) struct Placed {
    pub at: Point<f64, Physical>,
    pub src: Option<Rectangle<f64, Logical>>,
    pub size: Option<Size<i32, Logical>>,
}

impl Uploads {
    /// `frame` as an element drawn by `renderer` where `placed` puts it, brought up to date with
    /// what the surface drew since this compositor last drew it: `None` where the frame stands no
    /// longer or shows nothing yet.
    pub(crate) fn element<R>(
        &self,
        renderer: &mut R,
        frame: &HostFrame,
        placed: Placed,
    ) -> Result<Option<OutputRenderElement<R>>, R::Error>
    where
        R: Renderer + ImportAll + ImportMem + ImportDma,
        R::TextureId: Clone + Send + 'static,
    {
        self.refresh(frame);
        let id = frame.identity();
        let mut frames = self.frames.borrow_mut();
        let Some(drawn) = frames.get_mut(&id) else {
            return Ok(None);
        };
        match &mut drawn.shown {
            Shown::Memory(buffer) => MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                placed.at,
                buffer,
                None,
                placed.src,
                placed.size,
                Kind::Unspecified,
            )
            .map(|element| Some(OutputRenderElement::Memory(element))),
            Shown::Gpu(gpu) => {
                let Some(texture) = gpu.texture(renderer)? else {
                    return Ok(None);
                };
                Ok(Some(OutputRenderElement::Hosted(
                    TextureRenderElement::from_texture_render_buffer(
                        placed.at,
                        texture,
                        None,
                        placed.src,
                        placed.size,
                        Kind::Unspecified,
                    ),
                )))
            }
        }
    }

    /// Take what `frame` changed since it was last drawn here: into its memory buffer for a frame
    /// drawn on the CPU, as the buffer to show and the parts to draw again for one drawn on the
    /// GPU.
    fn refresh(&self, frame: &HostFrame) {
        let Some(changed) = frame.take_changed() else {
            return;
        };
        let id = frame.identity();
        let mut frames = self.frames.borrow_mut();
        if !frames.contains_key(&id) {
            frames.retain(|_, drawn| drawn.frame.alive());
            let (width, height) = (wide(frame.size().0), wide(frame.size().1));
            let shown = match changed.buffer() {
                Some((buffers, _)) => Shown::Gpu(OnGpu::new(buffers, (width, height))),
                None => Shown::Memory(MemoryRenderBuffer::new(
                    Fourcc::Abgr8888,
                    (width, height),
                    1,
                    Transform::Normal,
                    None,
                )),
            };
            frames.insert(
                id,
                Drawn {
                    frame: frame.watch(),
                    shown,
                },
            );
        }
        let Some(drawn) = frames.get_mut(&id) else {
            return;
        };
        let regions: Vec<Rectangle<i32, Buffer>> =
            changed.regions().iter().filter_map(rectangle).collect();
        // What nothing under shows through: no layer beneath it is drawn there, and no pixel it
        // covers is cleared.
        let opaque = changed
            .opaque()
            .map(|opaque| opaque.iter().filter_map(rectangle).collect::<Vec<_>>());
        match (&mut drawn.shown, changed.pixels()) {
            (Shown::Memory(buffer), Some(pixels)) => {
                let width = frame.size().0 as usize;
                let mut context = buffer.render();
                let copied = context.draw(|memory: &mut [u8]| {
                    for region in changed.regions() {
                        for row in region.y..region.y.saturating_add(region.height) {
                            let start = (row as usize)
                                .saturating_mul(width)
                                .saturating_add(region.x as usize)
                                .saturating_mul(4);
                            let end =
                                start.saturating_add((region.width as usize).saturating_mul(4));
                            if let (Some(to), Some(from)) =
                                (memory.get_mut(start..end), pixels.get(start..end))
                            {
                                to.copy_from_slice(from);
                            }
                        }
                    }
                    Ok::<_, std::convert::Infallible>(regions)
                });
                if let Err(never) = copied {
                    match never {}
                }
                if opaque.is_some() {
                    context.update_opaque_regions(opaque);
                }
            }
            (Shown::Gpu(gpu), None) => {
                gpu.shown = changed.buffer().map(|(_, index)| index);
                gpu.pending.extend(regions);
                gpu.opaque = opaque;
            }
            // A frame is drawn where it was first drawn for as long as it stands.
            _ => {}
        }
    }
}

impl OnGpu {
    fn new(buffers: &Arc<HostBuffers>, size: (i32, i32)) -> Self {
        let dmabufs = buffers
            .buffers
            .iter()
            .filter_map(|buffer| {
                let mut builder = Dmabuf::builder(
                    size,
                    Fourcc::try_from(buffers.fourcc).ok()?,
                    Modifier::from(buffers.modifier),
                    DmabufFlags::empty(),
                );
                builder.add_plane(buffer.fd.try_clone().ok()?, 0, buffer.offset, buffer.stride);
                builder.build()
            })
            .collect();
        Self {
            dmabufs,
            shown: None,
            pending: Vec::new(),
            opaque: None,
            texture: None,
        }
    }

    /// What `renderer` draws the frame from: the shown buffer imported there, with the parts that
    /// changed since it last drew it. A frame drawn by another renderer than the last is drawn whole.
    fn texture<R>(
        &mut self,
        renderer: &mut R,
    ) -> Result<Option<&TextureRenderBuffer<R::TextureId>>, R::Error>
    where
        R: ImportDma,
        R::TextureId: Clone + Send + 'static,
    {
        type Held<T> = (ContextId<T>, TextureRenderBuffer<T>);
        let Some(dmabuf) = self.shown.and_then(|index| self.dmabufs.get(index)) else {
            return Ok(None);
        };
        // The renderer keeps its import of a dma-buf for as long as the dma-buf stands.
        let imported = renderer.import_dmabuf(dmabuf, None)?;
        let context = renderer.context_id();
        let damage = std::mem::take(&mut self.pending);
        if let Some((_, held)) = self
            .texture
            .as_mut()
            .and_then(|held| held.downcast_mut::<Held<R::TextureId>>())
            .filter(|(drawn_by, _)| *drawn_by == context)
        {
            let mut drawing = held.render();
            let _ = drawing.draw(|texture| {
                *texture = imported;
                Ok::<_, std::convert::Infallible>(damage)
            });
            drawing.update_opaque_regions(self.opaque.clone());
        } else {
            let held = TextureRenderBuffer::from_texture(
                renderer,
                imported,
                1,
                Transform::Normal,
                self.opaque.clone(),
            );
            self.texture = Some(Box::new((context, held)));
        }
        Ok(self
            .texture
            .as_ref()
            .and_then(|held| held.downcast_ref::<Held<R::TextureId>>())
            .map(|(_, held)| held))
    }
}

fn wide(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn rectangle(region: &Region) -> Option<Rectangle<i32, Buffer>> {
    Some(Rectangle::new(
        (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
        (
            i32::try_from(region.width).ok()?,
            i32::try_from(region.height).ok()?,
        )
            .into(),
    ))
}
