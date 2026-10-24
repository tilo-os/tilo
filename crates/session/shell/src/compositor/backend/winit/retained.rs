//! The nested backend retains its Vello target, not swapchain images. Sparse
//! rendering is allowed only over the exact successfully rendered predecessor.

use crate::shell::render::paint::SceneDamage;
use vello::{RenderParams, Renderer, Scene, wgpu};

#[derive(Default)]
pub(super) struct TargetHistory {
    revision: Option<u64>,
    base_color: Option<vello::peniko::Color>,
}

pub(super) struct Target<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut Renderer,
    pub view: &'a wgpu::TextureView,
    pub params: &'a RenderParams,
}

#[derive(Debug)]
pub(super) struct RenderWork {
    pub pixels: u64,
    pub sparse: bool,
}

impl TargetHistory {
    pub(super) fn clear(&mut self) {
        self.revision = None;
        self.base_color = None;
    }

    /// Complete the render and its overflow check before its image can be
    /// presented. A failure discards authority, including when the old target
    /// was partially overwritten. The next frame must repair it densely.
    pub(super) fn render(
        &mut self,
        mut target: Target<'_>,
        scene: &Scene,
        damage: Option<SceneDamage<'_>>,
    ) -> Result<RenderWork, String> {
        let regions = damage.and_then(|damage| {
            (damage.revision != 0
                && damage.previous_revision.is_some()
                && self.revision == damage.previous_revision
                && self.base_color == Some(target.params.base_color))
            .then_some(damage.regions)
            .flatten()
        });
        self.clear();
        let work = if regions.is_some_and(<[vello::FineDamageRect]>::is_empty) {
            RenderWork {
                pixels: 0,
                sparse: true,
            }
        } else {
            render_complete(&mut target, scene, regions)?
        };
        self.revision = damage
            .filter(|damage| damage.revision != 0)
            .map(|damage| damage.revision);
        self.base_color = Some(target.params.base_color);
        Ok(work)
    }
}

fn render_complete(
    target: &mut Target<'_>,
    scene: &Scene,
    regions: Option<&[vello::FineDamageRect]>,
) -> Result<RenderWork, String> {
    // Overflow recovery is bounded, as are all fence waits: a suspended or
    // lost device must not hold the presentation thread indefinitely.
    for _ in 0..4 {
        let sparse = match regions {
            Some(regions) => target
                .renderer
                .render_to_texture_with_damage(
                    target.device,
                    target.queue,
                    scene,
                    target.view,
                    target.params,
                    regions,
                )
                .map_err(|error| error.to_string())?,
            None => None,
        };
        if sparse.is_none() {
            target
                .renderer
                .render_to_texture(
                    target.device,
                    target.queue,
                    scene,
                    target.view,
                    target.params,
                )
                .map_err(|error| error.to_string())?;
        }
        let work = RenderWork {
            pixels: sparse.as_ref().map_or_else(
                || u64::from(target.params.width).saturating_mul(u64::from(target.params.height)),
                |stats| stats.written_pixels,
            ),
            sparse: sparse.is_some(),
        };
        if !settle(target)? {
            return Ok(work);
        }
    }
    Err("Vello bump buffers still overflow after four attempts".into())
}

fn settle(target: &mut Target<'_>) -> Result<bool, String> {
    for _ in 0..4 {
        target
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(1)),
            })
            .map_err(|error| error.to_string())?;
        match target
            .renderer
            .poll_bump_budget(target.device)
            .map_err(|error| error.to_string())?
        {
            vello::BumpBudgetPoll::Complete { overflowed } => return Ok(overflowed),
            vello::BumpBudgetPoll::Pending => std::thread::yield_now(),
        }
    }
    Err("Vello bump counter readback did not complete".into())
}

#[cfg(test)]
mod tests;
