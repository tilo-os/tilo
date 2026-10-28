use super::*;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    history: TargetHistory,
    params: RenderParams,
}

impl Harness {
    fn new() -> Self {
        let mut context = ui::gpu::RenderContext::new();
        let index =
            pollster::block_on(context.device(None)).expect("GPU required for winit target oracle");
        let device = context.devices[index].device.clone();
        let queue = context.devices[index].queue.clone();
        let renderer = Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .expect("Vello renderer");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("winit retained-target oracle"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut harness = Self {
            device,
            queue,
            renderer,
            texture,
            view,
            history: TargetHistory::default(),
            params: RenderParams {
                width: 64,
                height: 64,
                base_color: Color::WHITE,
                antialiasing_method: vello::AaConfig::Area,
            },
        };
        harness.warm_pipelines();
        harness
    }

    // Software adapters may JIT shaders on first submission for longer than
    // the live presenter's one-second fence budget. Warm both paths before
    // testing steady-state target reuse; history stays invalid throughout.
    fn warm_pipelines(&mut self) {
        let scene = scene(18.0);
        self.renderer
            .render_to_texture(&self.device, &self.queue, &scene, &self.view, &self.params)
            .expect("warm dense pipeline");
        self.finish_warm();
        self.renderer
            .render_to_texture_with_damage(
                &self.device,
                &self.queue,
                &scene,
                &self.view,
                &self.params,
                &[vello::FineDamageRect {
                    x: 16,
                    y: 16,
                    width: 16,
                    height: 16,
                }],
            )
            .expect("warm sparse pipeline")
            .expect("sparse fine must be available");
        self.finish_warm();
    }

    fn finish_warm(&mut self) {
        for _ in 0..4 {
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(30)),
                })
                .expect("pipeline warmup fence");
            match self
                .renderer
                .poll_bump_budget(&self.device)
                .expect("pipeline warmup counters")
            {
                vello::BumpBudgetPoll::Complete { overflowed } => {
                    assert!(!overflowed, "minimal warmup scene must fit");
                    return;
                }
                vello::BumpBudgetPoll::Pending => std::thread::yield_now(),
            }
        }
        panic!("pipeline warmup counters did not complete");
    }

    fn render(&mut self, scene: &Scene, damage: SceneDamage<'_>) -> RenderWork {
        self.history
            .render(
                Target {
                    device: &self.device,
                    queue: &self.queue,
                    renderer: &mut self.renderer,
                    view: &self.view,
                    params: &self.params,
                },
                scene,
                Some(damage),
            )
            .expect("winit production target render")
    }

    fn pixels(&self) -> Vec<u8> {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("winit target pixels"),
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).expect("map reply");
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .expect("GPU readback fence");
        rx.recv().expect("map callback").expect("map result");
        let pixels = buffer.slice(..).get_mapped_range().to_vec();
        buffer.unmap();
        pixels
    }
}

fn scene(x: f64) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::BLACK,
        None,
        &Rect::new(x, 18.0, x + 4.0, 22.0),
    );
    scene
}

fn damage(
    revision: u64,
    previous: Option<u64>,
    regions: Option<&[vello::FineDamageRect]>,
) -> SceneDamage<'_> {
    SceneDamage {
        revision,
        previous_revision: previous,
        regions,
    }
}

#[test]
fn winit_target_repaints_only_a_proven_predecessor_and_matches_dense_pixels() {
    let mut retained = Harness::new();
    let mut dense = Harness::new();
    let rects = [vello::FineDamageRect {
        x: 16,
        y: 16,
        width: 16,
        height: 16,
    }];
    let first = scene(18.0);
    assert!(
        !retained
            .render(&first, damage(1, None, Some(&rects)))
            .sparse
    );
    let next = scene(24.0);
    let work = retained.render(&next, damage(2, Some(1), Some(&rects)));
    assert!(work.sparse);
    assert!(work.pixels <= 16 * 16);
    dense.render(&next, damage(2, None, None));
    assert_eq!(retained.pixels(), dense.pixels());
    let kept = retained.render(&next, damage(3, Some(2), Some(&[])));
    assert_eq!(kept.pixels, 0);
    assert_eq!(retained.pixels(), dense.pixels());
    // A missing predecessor must repair the entire target, even if the
    // producer mistakenly supplied an empty change set.
    let changed = scene(40.0);
    assert!(
        !retained
            .render(&changed, damage(4, Some(99), Some(&[])))
            .sparse
    );
    dense.render(&changed, damage(4, None, None));
    assert_eq!(retained.pixels(), dense.pixels());
    // Resize/reconfigure/acquire failure all discard the same target authority.
    retained.history.clear();
    assert!(
        !retained
            .render(&first, damage(5, Some(4), Some(&rects)))
            .sparse
    );
    // An unversioned full boundary cannot seed a subsequent sparse frame.
    retained.render(&changed, damage(0, None, None));
    assert!(
        !retained
            .render(&first, damage(6, Some(0), Some(&rects)))
            .sparse
    );
    // The clear colour is not in the scene's encoding; changing it is dense.
    retained.params.base_color = Color::from_rgb8(30, 60, 90);
    dense.params.base_color = retained.params.base_color;
    assert!(
        !retained
            .render(&next, damage(7, Some(6), Some(&rects)))
            .sparse
    );
    dense.render(&next, damage(7, None, None));
    assert_eq!(retained.pixels(), dense.pixels());
    // Unsupported sparse dispatch falls back to the complete production render.
    retained.renderer.set_sparse_fine_enabled(false);
    assert!(
        !retained
            .render(&first, damage(8, Some(7), Some(&rects)))
            .sparse
    );
    dense.render(&first, damage(8, None, None));
    assert_eq!(retained.pixels(), dense.pixels());
}
