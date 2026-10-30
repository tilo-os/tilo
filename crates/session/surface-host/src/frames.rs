//! Where a hosted surface's frame is drawn.
//!
//! A surface app's scene is drawn the way every toolkit app's frame is: by
//! the toolkit's own renderer ([`ui::offscreen::OffscreenRenderer`]), on
//! whatever adapter it picks. Only what changed is drawn, so a moved cursor,
//! a new time or a resized panel costs what it changed rather than the whole
//! surface.

use tilo_compositor::{HostFrame, Region};
use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::Color;

/// The colour a surface draws on: nothing, so the desktop shows through
/// wherever the app painted nothing.
const TRANSPARENT: Color = Color::new([0.0, 0.0, 0.0, 0.0]);

/// One hosted surface's frames: what it drew last, and what draws its next.
pub(crate) struct Frames {
    size: (u32, u32),
    target: Option<Gpu>,
    /// What the compositor draws: the surface's pixels, changed where it
    /// changed.
    frame: HostFrame,
}

impl Frames {
    /// Frames for a surface of `size` physical pixels. Nothing is drawn
    /// before [`Self::present`].
    pub(crate) fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            target: Gpu::new(size),
            frame: HostFrame::new(size),
        }
    }

    /// The surface is now `size` physical pixels: the frames drawn for the
    /// old size are not its own.
    pub(crate) fn resize(&mut self, size: (u32, u32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.target = Gpu::new(size);
        self.frame = HostFrame::new(size);
    }

    /// The frame as the compositor shows it.
    pub(crate) fn frame(&self) -> HostFrame {
        self.frame.clone()
    }

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The alpha of one pixel of the last frame drawn, if it lies inside.
    pub(crate) fn alpha(&self, x: u32, y: u32) -> Option<u8> {
        let pixels = self.target.as_ref()?.pixels.as_slice();
        alpha_of(pixels, self.size, x, y)
    }

    /// Draw `scene` (physical pixels) where `damage` says it changed (`None`:
    /// all of it) and copy what was drawn into the frame, telling it what
    /// changed.
    pub(crate) fn present(&mut self, scene: &Scene, damage: Option<&[Rect]>) {
        let Some(target) = self.target.as_mut() else {
            return;
        };
        let Some(drawn) = target.draw(scene, damage) else {
            return;
        };
        self.frame.set_pixels(&target.pixels, &drawn);
    }
}

/// The renderer and the frame it drew last.
struct Gpu {
    renderer: OffscreenRenderer,
    /// Premultiplied RGBA, `width * 4` bytes a row, of the whole surface.
    pixels: Vec<u8>,
}

impl Gpu {
    /// The renderer for a surface of `size` physical pixels, if the toolkit
    /// has an adapter to draw it on.
    fn new(size: (u32, u32)) -> Option<Self> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let renderer = match OffscreenRenderer::try_new_live(size.0, size.1) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::warn!(%error, "a hosted surface has no renderer to draw on");
                return None;
            }
        };
        Some(Self {
            pixels: vec![0; length(size)],
            renderer,
        })
    }

    /// Draw `scene` where `damage` says it changed, keeping the drawn frame.
    /// The regions drawn, or `None` where nothing changed.
    fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>) -> Option<Vec<Region>> {
        let renderer = &mut self.renderer;
        let changed = match damage.map(|rects| regions(rects, renderer.width, renderer.height)) {
            Some(changed) if changed.is_empty() => return None,
            Some(changed) => {
                let rects: Vec<Rect> = changed
                    .iter()
                    .map(|region| {
                        Rect::new(
                            f64::from(region.x),
                            f64::from(region.y),
                            f64::from(region.x.saturating_add(region.width)),
                            f64::from(region.y.saturating_add(region.height)),
                        )
                    })
                    .collect();
                renderer.render_damage_regions_gpu_wait(scene, TRANSPARENT, &rects);
                changed
            }
            None => {
                renderer.render_gpu_wait(scene, TRANSPARENT);
                vec![Region::new(0, 0, renderer.width, renderer.height)]
            }
        };
        self.pixels = renderer.readback_current();
        (self.pixels.len() == length((renderer.width, renderer.height))).then_some(changed)
    }
}

/// The pixels of a `size` surface.
fn length(size: (u32, u32)) -> usize {
    (size.0 as usize)
        .saturating_mul(size.1 as usize)
        .saturating_mul(4)
}

/// The alpha of one pixel of `pixels`, a `size` surface, if it lies inside.
pub(crate) fn alpha_of(pixels: &[u8], size: (u32, u32), x: u32, y: u32) -> Option<u8> {
    if x >= size.0 || y >= size.1 {
        return None;
    }
    let index = (y as usize)
        .checked_mul(size.0 as usize)?
        .checked_add(x as usize)?
        .checked_mul(4)?
        .checked_add(3)?;
    pixels.get(index).copied()
}

/// `rects` as whole-pixel regions of a `width` by `height` target: rounded
/// out, clipped, and merged where they overlap so no pixel is drawn twice.
pub(crate) fn regions(rects: &[Rect], width: u32, height: u32) -> Vec<Region> {
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut merged: Vec<Rect> = Vec::new();
    for rect in rects {
        let mut rect = rect.expand().intersect(bounds);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        while let Some(index) = merged
            .iter()
            .position(|other| other.intersect(rect).area() > 0.0)
        {
            rect = rect.union(merged.swap_remove(index));
        }
        merged.push(rect);
    }
    merged
        .into_iter()
        .map(|rect| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "whole-pixel rectangles clipped to the target"
            )]
            let region = Region::new(
                rect.x0 as u32,
                rect.y0 as u32,
                rect.width() as u32,
                rect.height() as u32,
            );
            region
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_is_rounded_out_clipped_and_merged() {
        let rects = [
            Rect::new(1.5, 1.5, 4.2, 4.2),
            Rect::new(3.0, 3.0, 6.0, 6.0),
            Rect::new(90.0, 90.0, 120.0, 120.0),
            Rect::new(-5.0, -5.0, -1.0, -1.0),
        ];
        let mut out = regions(&rects, 100, 100);
        out.sort_by_key(|region| (region.x, region.y));
        assert_eq!(
            out,
            vec![Region::new(1, 1, 5, 5), Region::new(90, 90, 10, 10)]
        );
    }
}
