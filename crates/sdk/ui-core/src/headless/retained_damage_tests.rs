//! A host that keeps the frame it presented repaints only where the next frame changed
//! (`retained_raster`): typing repaints the composer, and anything the parts cannot prove (a resize)
//! repaints the whole frame. The retained target must always equal a full render of the same frame.

use super::*;
use crate::retained_raster::{
    FrameContent, RasterCheck, Readback, Repaint, RetainedFrames, RetainedRaster, RetainedTarget,
    TargetKey, Unknown, culled_for_damage,
};
use crate::widget::{
    Column, Dropdown, GroupRow, ScrollAxis, Scrollable, TabDescriptor, Text, TextInput,
    TileContent, Widget, WindowGroup, WindowWidth,
};
use std::cell::RefCell;
use std::rc::Rc;
use vello::kurbo::{Affine, Rect};

#[derive(Clone, Copy, Default)]
struct Places {
    messages: Rect,
    menu: Rect,
    composer: Rect,
}

struct Chat {
    root: Column,
    places: Rc<RefCell<Places>>,
}

impl Chat {
    fn new(places: Rc<RefCell<Places>>) -> Self {
        let messages = (0..80).fold(Column::new(), |column, i| {
            column.push(Box::new(Text::new(&format!("Message number {i}"))))
        });
        let root = Column::new()
            .push(Box::new(
                Scrollable::new(ScrollAxis::Vertical)
                    .with_height(320.0)
                    .with_child(Box::new(messages)),
            ))
            .push(Box::new(Dropdown::new(
                vec!["Everyone".into(), "Only me".into(), "Nobody".into()],
                "audience",
            )))
            .push(Box::new(TextInput::new(vec![crate::PathSegment::Field(
                "draft".into(),
            )])));
        Self { root, places }
    }
}

impl Widget for Chat {
    crate::testing::delegate_widget_methods!(
        root: sync,
        build_layout,
        apply_layout,
        handle_event,
        get_rect
    );
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.root.draw(scene, transform, text, theme, presence);
        let mut rects = Vec::new();
        self.root.for_each_child(&mut |child| {
            rects.push(transform.transform_rect_bbox(child.get_rect()));
        });
        if let [messages, menu, composer] = rects[..] {
            *self.places.borrow_mut() = Places {
                messages,
                menu,
                composer,
            };
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.root]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.root]
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.root.collect_damage(regions);
    }
    fn name(&self) -> &str {
        "Chat"
    }
}

crate::testing::test_app_widget!(Chat, "Chat");

const SIZE: (u32, u32) = (1200, 800);

fn desk_row(places: Rc<RefCell<Places>>) -> GroupRow {
    let chat = WindowGroup::new(
        100,
        vec![(
            TabDescriptor::new(1, "Chat"),
            TileContent::App(Box::new(Chat::new(places))),
        )],
        1,
        WindowWidth::Normal,
    );
    let notes = WindowGroup::new(
        200,
        vec![(
            TabDescriptor::new(2, "Notes"),
            TileContent::Stub {
                kind: crate::widget::AppKind::Settings,
                label: "Notes".into(),
            },
        )],
        2,
        WindowWidth::Normal,
    );
    GroupRow::new(vec![chat, notes])
}

fn desktop_of(root: impl Widget + 'static) -> FrameHarness {
    let mut harness = FrameHarness::for_root(crate::State::new(), root, SIZE, Theme::dark());
    harness.track_scene_parts(true);
    harness.set_coalesce_input_frames(true);
    harness.relayout();
    harness
}

fn desktop() -> (FrameHarness, Rc<RefCell<Places>>) {
    let places = Rc::new(RefCell::new(Places::default()));
    let harness = desktop_of(desk_row(Rc::clone(&places)));
    (harness, places)
}

fn key(harness: &FrameHarness, (width, height): (u32, u32), generation: u64) -> TargetKey {
    TargetKey {
        width,
        height,
        to_device: Affine::IDENTITY,
        base_color: harness.frame_render_input().background,
        generation,
    }
}

fn present(harness: &mut FrameHarness, frames: &mut RetainedFrames, key: TargetKey) -> Repaint {
    harness.poll_frame().unwrap();
    let scene = harness.frame_render_input().scene;
    let repaint = frames.repaint(scene, harness.scene_parts(), key);
    frames.presented(scene, harness.scene_parts(), key);
    repaint
}

fn centre(rect: Rect) -> (f64, f64) {
    let centre = rect.center();
    (centre.x, centre.y)
}

#[test]
fn a_typed_character_repaints_only_the_composer() {
    let (mut harness, places) = desktop();
    let mut frames = RetainedFrames::default();
    let key = key(&harness, SIZE, 0);
    assert_eq!(
        present(&mut harness, &mut frames, key),
        Repaint::Full(Unknown::NoFrame),
        "nothing was presented before"
    );
    let composer = places.borrow().composer;
    assert!(composer.area() > 0.0, "the composer is drawn: {composer:?}");
    let (x, y) = centre(composer);
    harness.click_at(x, y);
    present(&mut harness, &mut frames, key);
    for letter in ["h", "i", "!"] {
        harness.type_text(letter);
        let Repaint::Regions(regions) = present(&mut harness, &mut frames, key) else {
            panic!("typing {letter:?} repaints the whole frame");
        };
        assert!(!regions.is_empty(), "typing {letter:?} repaints nothing");
        let reach = composer.inflate(20.0, 20.0);
        for region in &regions {
            assert_eq!(
                region.intersect(reach),
                *region,
                "typing {letter:?} repainted {region:?} outside the composer {composer:?}"
            );
        }
    }
    assert_eq!(
        present(&mut harness, &mut frames, key),
        Repaint::Regions(Vec::new()),
        "an unchanged frame repaints nothing"
    );
}

#[test]
fn a_resize_repaints_the_whole_frame() {
    let (mut harness, _) = desktop();
    let mut frames = RetainedFrames::default();
    let first = key(&harness, SIZE, 0);
    present(&mut harness, &mut frames, first);
    let same = harness.frame_render_input().scene;
    let parts = harness.scene_parts();
    assert_eq!(
        frames.repaint(same, parts, key(&harness, SIZE, 1)),
        Repaint::Full(Unknown::Target)
    );
    let scaled = TargetKey {
        to_device: Affine::scale(2.0),
        ..key(&harness, SIZE, 0)
    };
    assert_eq!(
        frames.repaint(same, parts, scaled),
        Repaint::Full(Unknown::Target)
    );

    harness.resize((1000, 700)).unwrap();
    let resized = key(&harness, (1000, 700), 0);
    assert_eq!(
        present(&mut harness, &mut frames, resized),
        Repaint::Full(Unknown::Target)
    );
}

/// A host's retained target on the shared GPU: each frame repaints only where it changed, what it
/// repaints is culled to the parts that can reach the damage (as the hosts cull it), and every repaint
/// is compared with a full render of the frame.
struct Retained {
    device: vello::wgpu::Device,
    queue: vello::wgpu::Queue,
    renderer: vello::Renderer,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
    raster: RetainedRaster,
    frames: RetainedFrames,
    /// The renderer's collected overflows, as the hosts keep them: a frame drawn while it collected one
    /// has geometry missing.
    overflows: crate::render_bump_recovery::Overflows,
    partial: usize,
    sparse: bool,
}

impl Retained {
    fn new(sparse: bool) -> Self {
        let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the raster check");
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
        // Software rasterization sums a tile's segments in scheduling order; a
        // fixed order leaves the comparison only stale or missing pixels.
        renderer
            .enable_canonical_segment_order(&gpu.device)
            .unwrap();
        renderer.set_sparse_fine_enabled(sparse);
        let texture = gpu.device.create_texture(&vello::wgpu::TextureDescriptor {
            label: Some("retained-session-target"),
            size: vello::wgpu::Extent3d {
                width: SIZE.0,
                height: SIZE.1,
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
            overflows: crate::render_bump_recovery::Overflows::default(),
            partial: 0,
            sparse,
        }
    }

    /// Repaint what this frame changed onto the target, then compare the target with a full render of
    /// the frame.
    fn step(&mut self, harness: &mut FrameHarness, key: TargetKey, what: &str) {
        let sparse = self.sparse;
        let Some(readback) = self.step_report(harness, key) else {
            return;
        };
        match readback {
            Readback::Differing(differences) => assert_eq!(
                differences.count, 0,
                "after {what} (sparse {sparse}), repainting left stale pixels at {:?}",
                differences.bounds
            ),
            // A check that could not run proves nothing either way, and the
            // test says so rather than calling the frame stale.
            Readback::Unreadable(why) => panic!(
                "after {what} (sparse {sparse}), the check could not read its frames back: {why}"
            ),
        }
    }

    fn step_report(&mut self, harness: &mut FrameHarness, key: TargetKey) -> Option<Readback> {
        self.frames
            .note_scratch(self.overflows.scratch(&self.renderer));
        let repaint = present(harness, &mut self.frames, key);
        let input = harness.frame_render_input();
        let (damage, full_redraw) = match &repaint {
            Repaint::Full(_) => (&[][..], true),
            Repaint::Regions(regions) => (regions.as_slice(), false),
        };
        let culled = culled_for_damage(input.scene, harness.scene_parts(), key, damage);
        let mut physical = vello::Scene::new();
        physical.append(culled.as_ref().unwrap_or(input.scene), Some(key.to_device));
        self.raster
            .rasterize(
                (&self.device, &self.queue),
                Some(&mut self.renderer),
                RetainedTarget {
                    texture: &self.texture,
                    view: &self.view,
                },
                FrameContent {
                    scene: &physical,
                    width: SIZE.0,
                    height: SIZE.1,
                    base_color: input.background,
                    damage_regions: damage,
                    scroll_copies: &[],
                    full_redraw,
                    backdrop: &crate::backdrop::BackdropFrame::default(),
                },
            )
            .0
            .unwrap();
        if full_redraw {
            return None;
        }
        self.partial += 1;
        let whole = culled.is_some().then(|| {
            let mut whole = vello::Scene::new();
            whole.append(input.scene, Some(key.to_device));
            whole
        });
        let scratch = self.overflows.scratch(&self.renderer);
        let check = RasterCheck::start(
            (&self.device, &self.queue),
            &mut self.renderer,
            RetainedTarget {
                texture: &self.texture,
                view: &self.view,
            },
            whole.as_ref().unwrap_or(&physical),
            SIZE,
            input.background,
            scratch,
        )
        .unwrap();
        // The host's counters, as a host that can wait for the GPU reads
        // them: whether the render that drew this frame dropped geometry is
        // only known from the readback, after the frame was drawn.
        let overflowed = self.renderer.settle_bump_budget(&self.device);
        self.overflows
            .observe(Ok(vello::BumpBudgetPoll::Complete { overflowed }));
        // Another test polling the shared device may be the one that runs
        // the readback's callbacks, after this poll returns.
        loop {
            let _ = self.device.poll(vello::wgpu::PollType::wait_indefinitely());
            let scratch = self.overflows.scratch(&self.renderer);
            if let Some(readback) = check.differences(scratch) {
                return Some(readback);
            }
            std::thread::yield_now();
        }
    }
}

/// Every frame of a session repainted in part equals a full render of it, through Vello's sparse fine
/// pass and through a window around the damage (the web build's only way).
#[test]
fn a_session_repainted_in_part_matches_full_renders() {
    session_matches_full_renders(true);
    session_matches_full_renders(false);
}

fn session_matches_full_renders(sparse: bool) {
    let mut target = Retained::new(sparse);
    let (mut harness, places) = desktop();
    let key = key(&harness, SIZE, 0);
    let mut step = |harness: &mut FrameHarness, what: &str| target.step(harness, key, what);
    step(&mut harness, "the first frame");

    let (x, y) = centre(places.borrow().composer);
    harness.click_at(x, y);
    step(&mut harness, "focusing the composer");
    for letter in ["h", "e", "y"] {
        harness.type_text(letter);
        step(&mut harness, "typing");
    }

    let (x, y) = centre(places.borrow().messages);
    harness.click_at(x, y);
    step(&mut harness, "clicking the messages");
    for _ in 0..3 {
        harness.scroll(0.0, -3.0);
        step(&mut harness, "scrolling the messages");
    }

    let (x, y) = centre(places.borrow().menu);
    harness.click_at(x, y);
    step(&mut harness, "opening the menu");
    harness.key("ArrowDown");
    step(&mut harness, "moving through the menu");
    harness.key("Escape");
    step(&mut harness, "closing the menu");

    harness.root_mut::<GroupRow>().reorder_group_by_id(100, 1);
    harness.relayout();
    step(&mut harness, "moving the chat window after the other");
    harness.root_mut::<GroupRow>().reorder_group_by_id(100, 0);
    harness.relayout();
    step(&mut harness, "moving the chat window back");
    harness.type_text("!");
    step(&mut harness, "typing after the move");

    assert!(
        target.partial >= 3,
        "the session repainted in part {} times",
        target.partial
    );
}

/// A renderer that dropped geometry from a render may have dropped it from the frame the target shows
/// or from the check's render of it: a difference is then no proof of a stale frame.
#[test]
fn a_difference_after_lost_geometry_proves_no_stale_frame() {
    const SIDE: (u32, u32) = (320, 240);
    const STALE: vello::kurbo::Rect = vello::kurbo::Rect {
        x0: 40.0,
        y0: 40.0,
        x1: 90.0,
        y1: 80.0,
    };
    let base = vello::peniko::Color::from_rgb8(30, 30, 40);
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the raster check");
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
    let texture = gpu.device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("retained-stale-cause-target"),
        size: vello::wgpu::Extent3d {
            width: SIDE.0,
            height: SIDE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING
            | vello::wgpu::TextureUsages::COPY_SRC
            | vello::wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
    let target = RetainedTarget {
        texture: &texture,
        view: &view,
    };
    // A frame that drew `STALE`, and the frame after it, which removed it
    // but repainted a region that does not reach it: the target keeps it.
    let mut with_stale = vello::Scene::new();
    with_stale.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        vello::peniko::Color::WHITE,
        None,
        &STALE,
    );
    let without_stale = vello::Scene::new();
    let mut raster = RetainedRaster::new(false);
    let mut draw = |scene: &vello::Scene, damage: &[Rect]| {
        raster
            .rasterize(
                (&gpu.device, &gpu.queue),
                Some(&mut renderer),
                target,
                FrameContent {
                    scene,
                    width: SIDE.0,
                    height: SIDE.1,
                    base_color: base,
                    damage_regions: damage,
                    scroll_copies: &[],
                    full_redraw: damage.is_empty(),
                    backdrop: &crate::backdrop::BackdropFrame::default(),
                },
            )
            .0
            .unwrap();
    };
    draw(&with_stale, &[]);
    draw(&without_stale, &[Rect::new(200.0, 200.0, 240.0, 230.0)]);
    let mut overflows = crate::render_bump_recovery::Overflows::default();
    let before = overflows.scratch(&renderer);
    let check = RasterCheck::start(
        (&gpu.device, &gpu.queue),
        &mut renderer,
        target,
        &without_stale,
        SIDE,
        base,
        before,
    )
    .unwrap();
    let readback = loop {
        let _ = gpu.device.poll(vello::wgpu::PollType::wait_indefinitely());
        if let Some(readback) = check.differences(overflows.scratch(&renderer)) {
            break readback;
        }
        std::thread::yield_now();
    };
    let Readback::Differing(differences) = readback else {
        panic!("a target left stale was not compared: {readback:?}");
    };
    assert!(
        differences.count > 0,
        "the frame that removed the rectangle repainted a region away from it: \
         the target shows it still"
    );

    // The same comparison, with the renderer having dropped geometry from a
    // render in between: a stale frame and a lost render look the same.
    let before = overflows.scratch(&renderer);
    let check = RasterCheck::start(
        (&gpu.device, &gpu.queue),
        &mut renderer,
        target,
        &without_stale,
        SIDE,
        base,
        before,
    )
    .unwrap();
    let scratch = gpu.device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("retained-stale-cause-scratch"),
        size: vello::wgpu::Extent3d {
            width: SIDE.0,
            height: SIDE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    let scratch_view = scratch.create_view(&vello::wgpu::TextureViewDescriptor::default());
    let busy = busy_scene(SIDE);
    renderer.set_bump_budget_limit(1 << 20);
    renderer
        .render_to_texture(
            &gpu.device,
            &gpu.queue,
            &busy,
            &scratch_view,
            &vello::RenderParams {
                base_color: base,
                width: SIDE.0,
                height: SIDE.1,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .unwrap();
    let overflowed = renderer.settle_bump_budget(&gpu.device);
    overflows.observe(Ok(vello::BumpBudgetPoll::Complete { overflowed }));
    assert!(
        overflows.collected() > 0,
        "a scene of this many segments overflows a megabyte of scratch"
    );
    let readback = loop {
        let _ = gpu.device.poll(vello::wgpu::PollType::wait_indefinitely());
        if let Some(readback) = check.differences(overflows.scratch(&renderer)) {
            break readback;
        }
        std::thread::yield_now();
    };
    let Readback::Unreadable(why) = readback else {
        panic!("a check that saw lost geometry judged the frame: {readback:?}");
    };
    assert!(
        why.contains("scratch changed while this frame was drawn"),
        "the check says what it could not prove: {why}"
    );
}

fn busy_scene((width, height): (u32, u32)) -> vello::Scene {
    let mut scene = vello::Scene::new();
    let mut seed = 1u32;
    let mut random = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed >> 8) / f64::from(1u32 << 24)
    };
    let stroke = vello::kurbo::Stroke::new(1.5);
    for _ in 0..30_000 {
        let (x, y) = (random() * f64::from(width), random() * f64::from(height));
        let line = vello::kurbo::Line::new((x, y), (x + 6.0, y + 4.0));
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            vello::peniko::Color::from_rgb8(200, 200, 210),
            None,
            &line,
        );
    }
    scene
}

struct Scrimmed {
    row: GroupRow,
    open: bool,
}

impl Widget for Scrimmed {
    crate::testing::delegate_widget_methods!(
        row: sync,
        build_layout,
        apply_layout,
        handle_event,
        get_rect
    );
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.row.draw(scene, transform, text, theme, presence);
        if self.open {
            let frame = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
            crate::overlay::scrim::modal(scene, transform, frame, 1.0, theme);
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.row]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.row]
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.row.collect_damage(regions);
    }
    fn name(&self) -> &str {
        "Scrimmed desk"
    }
}

crate::testing::test_app_widget!(Scrimmed, "Scrimmed desk");

fn scrimmed_desktop() -> (FrameHarness, Rc<RefCell<Places>>) {
    let places = Rc::new(RefCell::new(Places::default()));
    let desk = Scrimmed {
        row: desk_row(Rc::clone(&places)),
        open: false,
    };
    (desktop_of(desk), places)
}

/// A scrim dims every pixel of the frame. Closing it must repaint every pixel it dimmed: no frame may
/// keep the dim, not the one that closes it and not the unchanged frames after it.
#[test]
fn closing_a_scrim_repaints_the_frame_it_dimmed() {
    let (mut harness, places) = scrimmed_desktop();
    let key = key(&harness, SIZE, 0);
    let mut target = Retained::new(false);
    target.step(&mut harness, key, "the first frame");

    harness.root_mut::<Scrimmed>().open = true;
    harness.relayout();
    target.step(&mut harness, key, "opening the scrim");

    let (x, y) = centre(places.borrow().composer);
    harness.click_at(x, y);
    target.step(&mut harness, key, "focusing the composer under the scrim");
    harness.type_text("x");
    target.step(&mut harness, key, "typing under the scrim");

    harness.root_mut::<Scrimmed>().open = false;
    harness.relayout();
    target.step(&mut harness, key, "closing the scrim");
    target.step(&mut harness, key, "the frame after the scrim closed");
}

/// A note: a page that scrolls a block editor of many paragraphs, as a tile's app.
struct Note {
    page: Scrollable,
}

impl Widget for Note {
    crate::testing::delegate_widget_methods!(
        page: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.page]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.page]
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.page.collect_damage(regions);
    }
    fn name(&self) -> &str {
        "Note"
    }
}

crate::testing::test_app_widget!(Note, "Note");

/// A note of eight paragraphs in a window, drawn once with its scene parts tracked, and where its
/// editor is.
fn note_window() -> (FrameHarness, RetainedFrames, TargetKey, Rect) {
    let markdown = (0..8)
        .map(|i| format!("Paragraph number {i} of the note."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let note = Note {
        page: Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            crate::widget::BlockEditor::from_markdown(&markdown),
        )),
    };
    let window = WindowGroup::new(
        100,
        vec![(
            TabDescriptor::new(1, "Note"),
            TileContent::App(Box::new(note)),
        )],
        1,
        WindowWidth::Normal,
    );
    let mut harness = FrameHarness::for_root(
        crate::State::new(),
        GroupRow::new(vec![window]),
        SIZE,
        Theme::dark(),
    );
    harness.track_scene_parts(true);
    harness.set_coalesce_input_frames(true);
    harness.relayout();
    let mut frames = RetainedFrames::default();
    let key = key(&harness, SIZE, 0);
    present(&mut harness, &mut frames, key);
    let editor = harness
        .targets()
        .into_iter()
        .find(|(key, _)| key == "block_editor")
        .map(|(_, rect)| rect)
        .expect("the note's editor");
    assert!(editor.height() > 200.0, "the note is long: {editor:?}");
    (harness, frames, key, editor)
}

/// Each region of `repaint` is a few lines high, not the editor.
fn assert_lines(what: &str, repaint: Repaint, editor: Rect) {
    let Repaint::Regions(regions) = repaint else {
        panic!("{what} repaints the whole frame");
    };
    assert!(!regions.is_empty(), "{what} repaints nothing");
    for region in &regions {
        assert!(
            region.height() < 120.0,
            "{what} repainted {region:?}, more than a few lines of the editor {editor:?}"
        );
    }
}

/// A new paragraph at the end of a note grows its editor by a line: the tile repaints that line and the
/// caret, not the whole editor, whose own damage says where its blocks were and are.
#[test]
fn a_new_paragraph_repaints_its_line_not_the_editor() {
    let (mut harness, mut frames, key, editor) = note_window();
    harness.click_at(editor.x0 + 40.0, editor.y0 + 12.0);
    for _ in 0..8 {
        harness.key("ArrowDown");
    }
    harness.key("End");
    present(&mut harness, &mut frames, key);
    assert!(harness.ime_window().is_some(), "the click focuses the note");
    for (what, step) in [("Enter", "Enter"), ("a letter", "x"), ("another", "y")] {
        harness.key(step);
        let repaint = present(&mut harness, &mut frames, key);
        assert_lines(what, repaint, editor);
    }
}

/// A selection dragged down a note, within a paragraph and then across paragraphs, repaints the rows
/// whose selection changed with each move, not the editor.
#[test]
fn a_dragged_selection_repaints_the_rows_it_changed() {
    let (mut harness, mut frames, key, editor) = note_window();
    let scale = harness.input_scale;
    let move_to = |harness: &mut FrameHarness, x: f64, y: f64| {
        harness.send(&crate::synthetic_input::cursor_moved(x * scale, y * scale));
    };
    let (x, y) = (editor.x0 + 40.0, editor.y0 + 12.0);
    move_to(&mut harness, x, y);
    harness.send(&crate::synthetic_input::mouse_press());
    present(&mut harness, &mut frames, key);
    move_to(&mut harness, x + 60.0, y);
    let repaint = present(&mut harness, &mut frames, key);
    assert_lines("a drag along the paragraph", repaint, editor);
    for step in 1..5 {
        move_to(&mut harness, x + 60.0, y + 30.0 * f64::from(step));
        let repaint = present(&mut harness, &mut frames, key);
        assert_lines("a drag across the paragraphs", repaint, editor);
    }
    harness.send(&crate::synthetic_input::mouse_release());
}
