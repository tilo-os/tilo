//! The page keeps the frame it presented and repaints only where the next one changed: a menu the
//! bar opens over the bare desk of a person's own workspace, arriving, hovered and gone, must leave
//! the kept frame exactly what a full render of each frame draws, at a laptop's 2x density.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{ChromeIntent, OwnChip};
use tilo_desktop::desktop_host::{DeskLayer, DesktopChrome, NoticeTiles};
use tilo_desktop::launchpad::CollaborationLaunchpad;
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::Standing;
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{DesktopTree, Strip, StripId, Workspace, WorkspaceSlot};
use tilo_desktop_surface::host::chrome::{ChromeSource, Facts, HostChrome, Notices};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::headless::FrameHarness;
use ui::retained_raster::{
    FrameContent, RasterCheck, Readback, Repaint, RetainedFrames, RetainedRaster, RetainedTarget,
    TargetKey, culled_for_damage,
};
use ui::widget::{Stack, Widget as _};
use vello::kurbo::{Affine, Point};

const PERSON: &str = "ada";
/// The page in logical pixels, drawn at twice that.
const VIEW: (u32, u32) = (640, 400);
const SCALE: f64 = 2.0;
const DEVICE: (u32, u32) = (1280, 800);

struct Page {
    handle: DesktopHandle,
    authors: RefCell<Authors>,
    intents: RefCell<Vec<ChromeIntent>>,
}

impl ChromeSource for Page {
    fn handle(&self) -> &DesktopHandle {
        &self.handle
    }

    fn person(&self) -> &str {
        PERSON
    }

    fn with_standing<R>(&self, answer: impl FnOnce(&Standing<'_>) -> R) -> R {
        answer(&Standing {
            person: PERSON,
            // The workspace is their own: its menu offers renaming it and a new strip.
            role: Some(tilo_action_policy::Role::Owner),
            members: None,
        })
    }

    fn with_authors<R>(&self, answer: impl FnOnce(&mut Authors) -> R) -> R {
        answer(&mut self.authors.borrow_mut())
    }

    fn own_chip(&self) -> OwnChip {
        OwnChip {
            name: "Ada Lovelace".into(),
            rgb: (0xe0, 0x7a, 0x5f),
            identity: String::new(),
        }
    }

    fn notices(&self) -> Notices {
        Notices::default()
    }

    fn facts(&self, _facts: &mut Facts) {}

    fn take_intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}

/// A person's own workspace, one strip and no window: the bare desk.
fn own_desk() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Workspace");
    workspace.strips = vec![Strip::new(StripId(2), "Main")];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree
}

/// The page: the desk under the windows and the chrome over it, in one frame as the page draws
/// them.
fn page() -> (FrameHarness, Rc<Page>) {
    let tree = own_desk();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    handle.set_viewport_width(f64::from(VIEW.0));
    let page = Rc::new(Page {
        handle: handle.clone(),
        authors: RefCell::default(),
        intents: RefCell::default(),
    });
    let desk = DeskLayer::new(
        Box::new(surface),
        handle.clone(),
        CollaborationLaunchpad::new(),
        Rc::default(),
        Vec::new(),
    );
    let source = Rc::new(HostChrome::new(Rc::clone(&page)));
    let bar = TopBarChrome::from_tree(&tree).with_chrome(source.clone());
    let chrome = DesktopChrome::new(
        handle,
        "g".into(),
        source,
        bar,
        TiloMenu::new(AboutVariant::Browser, &AboutLinks::default()),
        false,
    );
    let root = Stack::new().push(Box::new(desk)).push(Box::new(chrome));
    let mut frame = FrameHarness::for_root(ui::state::State::new(), root, VIEW, ui::Theme::light());
    frame.track_scene_parts(true);
    frame.set_coalesce_input_frames(true);
    frame.relayout();
    (frame, page)
}

/// What the bar asked the page for, carried out as the page carries it out before its next frame.
fn carry_out(frame: &mut FrameHarness, page: &Page) {
    let intents = std::mem::take(&mut *page.intents.borrow_mut());
    if intents.is_empty() {
        return;
    }
    let mut stack = frame.root_mut::<Stack>();
    for child in stack.children_mut() {
        if let Some(chrome) = child
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<DesktopChrome>())
        {
            for intent in &intents {
                chrome.apply_intent(intent.clone());
            }
        }
    }
}

/// Where the reader is told the control named `name` stands.
fn control(frame: &FrameHarness, name: &str) -> Point {
    let bounds = frame
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            node.label()
                .filter(|label| label.starts_with(name))
                .and_then(|_| node.bounds())
        })
        .expect("the bar names the control");
    Point::new(bounds.x0.midpoint(bounds.x1), bounds.y0.midpoint(bounds.y1))
}

/// The page's kept frame on the GPU, repainted as the page repaints it.
struct Kept {
    device: vello::wgpu::Device,
    queue: vello::wgpu::Queue,
    renderer: vello::Renderer,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
    raster: RetainedRaster,
    frames: RetainedFrames,
    overflows: ui::render_bump_recovery::Overflows,
    partial: usize,
}

impl Kept {
    fn new() -> Self {
        let gpu = ui::offscreen::shared_gpu().expect("a GPU to keep the frame on");
        let mut renderer = vello::Renderer::new(
            &gpu.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .unwrap();
        renderer
            .enable_canonical_segment_order(&gpu.device)
            .unwrap();
        renderer.set_sparse_fine_enabled(false);
        let texture = gpu.device.create_texture(&vello::wgpu::TextureDescriptor {
            label: Some("kept-page"),
            size: vello::wgpu::Extent3d {
                width: DEVICE.0,
                height: DEVICE.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello::wgpu::TextureDimension::D2,
            format: vello::wgpu::TextureFormat::Rgba8Unorm,
            usage: vello::wgpu::TextureUsages::STORAGE_BINDING
                | vello::wgpu::TextureUsages::COPY_SRC
                | vello::wgpu::TextureUsages::TEXTURE_BINDING
                | vello::wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
        Self {
            device: gpu.device,
            queue: gpu.queue,
            renderer,
            texture,
            view,
            raster: RetainedRaster::new(false),
            frames: RetainedFrames::default(),
            overflows: ui::render_bump_recovery::Overflows::default(),
            partial: 0,
        }
    }

    /// Draw the next frame as the page does and say whether the kept frame is that frame.
    fn step(&mut self, frame: &mut FrameHarness, page: &Page, what: &str) {
        carry_out(frame, page);
        frame.poll_frame().unwrap();
        let input = frame.frame_render_input();
        let key = TargetKey {
            width: DEVICE.0,
            height: DEVICE.1,
            to_device: Affine::scale(SCALE),
            base_color: input.background,
            generation: 0,
        };
        self.frames
            .note_scratch(self.overflows.scratch(&self.renderer));
        let (repaint, copies) = self
            .frames
            .repaint_copying(input.scene, frame.scene_parts(), key);
        self.frames.presented(input.scene, frame.scene_parts(), key);
        let (damage, full_redraw) = match &repaint {
            Repaint::Full(_) => (&[][..], true),
            Repaint::Regions(regions) => (regions.as_slice(), false),
        };
        let culled = culled_for_damage(input.scene, frame.scene_parts(), key, damage);
        let mut physical = vello::Scene::new();
        physical.append(culled.as_ref().unwrap_or(input.scene), Some(key.to_device));
        let target = RetainedTarget {
            texture: &self.texture,
            view: &self.view,
        };
        self.raster
            .rasterize(
                (&self.device, &self.queue),
                Some(&mut self.renderer),
                target,
                FrameContent {
                    scene: &physical,
                    width: DEVICE.0,
                    height: DEVICE.1,
                    base_color: input.background,
                    damage_regions: damage,
                    scroll_copies: &copies,
                    full_redraw,
                    backdrop: &ui::backdrop::BackdropFrame::default(),
                },
            )
            .0
            .unwrap();
        if full_redraw {
            return;
        }
        self.partial += 1;
        let mut whole = vello::Scene::new();
        whole.append(input.scene, Some(key.to_device));
        let scratch = self.overflows.scratch(&self.renderer);
        let check = RasterCheck::start(
            (&self.device, &self.queue),
            &mut self.renderer,
            RetainedTarget {
                texture: &self.texture,
                view: &self.view,
            },
            &whole,
            DEVICE,
            input.background,
            scratch,
        )
        .unwrap();
        let overflowed = self.renderer.settle_bump_budget(&self.device);
        self.overflows
            .observe(Ok(vello::BumpBudgetPoll::Complete { overflowed }));
        let readback = loop {
            let _ = self.device.poll(vello::wgpu::PollType::wait_indefinitely());
            if let Some(readback) = check.differences(self.overflows.scratch(&self.renderer)) {
                break readback;
            }
            std::thread::yield_now();
        };
        match readback {
            Readback::Differing(differences) => assert_eq!(
                differences.count, 0,
                "after {what}, the kept frame differs from the frame at {:?}: {damage:?}",
                differences.bounds
            ),
            Readback::Unreadable(why) => {
                panic!("after {what}, the frames could not be read: {why}")
            }
        }
    }
}

#[test]
fn the_workspace_menu_over_a_bare_desk_leaves_the_kept_frame_whole() {
    ui::i18n::I18nManager::init("en-US");
    let (mut frame, page) = page();
    let mut kept = Kept::new();
    kept.step(&mut frame, &page, "the first frame");
    kept.step(&mut frame, &page, "a frame at rest");
    let name = control(&frame, "Workspace: workspaces");
    frame.click_at(name.x, name.y);
    for step in 0..30 {
        kept.step(
            &mut frame,
            &page,
            &format!("frame {step} of the menu arriving"),
        );
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    assert!(
        frame
            .a11y_nodes()
            .into_iter()
            .any(|(_, node)| node.label() == Some("Rename workspace")),
        "the workspace's name at {name:?} opened its menu: {:?}",
        frame
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(|label| (label.to_owned(), node.bounds())))
            .collect::<Vec<_>>()
    );
    for row in 0..5 {
        frame.send_host_event(&ui::synthetic_input::cursor_moved(
            name.x,
            name.y + 40.0 + f64::from(row) * 24.0,
        ));
        for step in 0..4 {
            kept.step(
                &mut frame,
                &page,
                &format!("hovering row {row}, frame {step}"),
            );
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }
    frame.key("Escape");
    for step in 0..30 {
        kept.step(
            &mut frame,
            &page,
            &format!("frame {step} of the menu leaving"),
        );
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    assert!(
        kept.partial >= 10,
        "repainted in part {} times",
        kept.partial
    );
}
