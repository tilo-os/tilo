//! What the compositor draws hosted surfaces' frames with: one buffer per frame, holding the pixels
//! that frame showed when it was last drawn, brought up to date with just the parts the surface
//! changed since.

use std::cell::{Ref, RefCell};
use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Buffer, Rectangle, Transform};

use tilo_session_core::surface::HostFrame;

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
    buffer: MemoryRenderBuffer,
}

impl Uploads {
    /// The buffer `frame` is drawn from, brought up to date with what the surface drew since this
    /// compositor last drew it: `None` where the frame stands no longer.
    pub(crate) fn draw(&self, frame: &HostFrame) -> Option<Ref<'_, MemoryRenderBuffer>> {
        self.refresh(frame);
        let id = frame.identity();
        Ref::filter_map(self.frames.borrow(), |frames| {
            frames.get(&id).map(|drawn| &drawn.buffer)
        })
        .ok()
    }

    /// Copy what `frame` changed since it was last drawn here into its buffer.
    fn refresh(&self, frame: &HostFrame) {
        let id = frame.identity();
        let mut frames = self.frames.borrow_mut();
        if !frames.contains_key(&id) {
            frames.retain(|_, drawn| drawn.frame.alive());
            frames.insert(
                id,
                Drawn {
                    frame: frame.watch(),
                    buffer: MemoryRenderBuffer::new(
                        Fourcc::Abgr8888,
                        (
                            i32::try_from(frame.size().0).unwrap_or(i32::MAX),
                            i32::try_from(frame.size().1).unwrap_or(i32::MAX),
                        ),
                        1,
                        Transform::Normal,
                        None,
                    ),
                },
            );
        }
        let Some(drawn) = frames.get_mut(&id) else {
            return;
        };
        let Some(changed) = frame.take_changed() else {
            return;
        };
        let width = frame.size().0 as usize;
        let mut context = drawn.buffer.render();
        let copied = context.draw(|memory: &mut [u8]| {
            for region in changed.regions() {
                for row in region.y..region.y.saturating_add(region.height) {
                    let start = (row as usize)
                        .saturating_mul(width)
                        .saturating_add(region.x as usize)
                        .saturating_mul(4);
                    let end = start.saturating_add((region.width as usize).saturating_mul(4));
                    if let (Some(to), Some(from)) =
                        (memory.get_mut(start..end), changed.pixels().get(start..end))
                    {
                        to.copy_from_slice(from);
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(
                changed
                    .regions()
                    .iter()
                    .filter_map(|region| {
                        Some(Rectangle::<i32, Buffer>::new(
                            (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                            (
                                i32::try_from(region.width).ok()?,
                                i32::try_from(region.height).ok()?,
                            )
                                .into(),
                        ))
                    })
                    .collect(),
            )
        });
        if let Err(never) = copied {
            match never {}
        }
        // What nothing under shows through: no layer beneath it is drawn there, and no pixel it
        // covers is cleared.
        if let Some(opaque) = changed.opaque() {
            context.update_opaque_regions(Some(
                opaque
                    .into_iter()
                    .filter_map(|region| {
                        Some(Rectangle::<i32, Buffer>::new(
                            (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                            (
                                i32::try_from(region.width).ok()?,
                                i32::try_from(region.height).ok()?,
                            )
                                .into(),
                        ))
                    })
                    .collect(),
            ));
        }
    }
}
