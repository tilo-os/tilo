//! A host that keeps the frame it presented repaints only where the next
//! frame changed (`retained_raster`): typing repaints the composer, and
//! anything the parts cannot prove (a resize) repaints the whole frame. The
//! retained target must always equal a full render of the same frame.

use super::*;
use crate::retained_raster::{
    FrameContent, RasterCheck, Repaint, RetainedFrames, RetainedRaster, RetainedTarget, TargetKey,
    Unknown,
};
use crate::widget::{
    Column, Dropdown, GroupRow, ScrollAxis, Scrollable, TabDescriptor, Text, TextInput,
    TileContent, Widget, WindowGroup, WindowWidth,
};
use std::cell::RefCell;
use std::rc::Rc;
use vello::kurbo::{Affine, Rect};

/// Where the chat's parts were last drawn, in window coordinates.
#[derive(Clone, Copy, Default)]
struct Places {
    messages: Rect,
    menu: Rect,
    composer: Rect,
}

/// A chat window: messages that scroll, a menu and a composer.
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

/// A desktop of two windows, the chat first, drawing its damage parts.
fn desktop() -> (FrameHarness, Rc<RefCell<Places>>) {
    let places = Rc::new(RefCell::new(Places::default()));
    let chat = WindowGroup::new(
        100,
        vec![(
            TabDescriptor::new(1, "Chat"),
            TileContent::App(Box::new(Chat::new(Rc::clone(&places)))),
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
    let mut harness = FrameHarness::for_root(
        crate::State::new(),
        GroupRow::new(vec![chat, notes]),
        SIZE,
        Theme::dark(),
    );
    harness.track_scene_parts(true);
    // As the browser does: inputs are laid out and drawn once, by the frame.
    harness.set_coalesce_input_frames(true);
    harness.relayout();
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

/// Bring the frame up to date and say what of it a retained target repaints,
/// as the browser host does.
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
        // The app's own damage reaches a little past its widgets.
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
    // The same frame on another target, or the target after its pixels were
    // lost, is not known to show the last frame.
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

/// Every frame of a session repainted in part equals a full render of it,
/// through Vello's sparse fine pass and through a window around the damage
/// (the web build's only way).
#[test]
fn a_session_repainted_in_part_matches_full_renders() {
    session_matches_full_renders(true);
    session_matches_full_renders(false);
}

fn session_matches_full_renders(sparse: bool) {
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the raster check");
    let (device, queue) = (&gpu.device, &gpu.queue);
    let mut renderer = vello::Renderer::new(
        device,
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
    renderer.enable_canonical_segment_order(device).unwrap();
    renderer.set_sparse_fine_enabled(sparse);
    let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
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
    let target = RetainedTarget {
        texture: &texture,
        view: &view,
    };
    let mut raster = RetainedRaster::new(false);
    let mut frames = RetainedFrames::default();
    let (mut harness, places) = desktop();
    let key = key(&harness, SIZE, 0);
    let mut partial = 0;
    let mut step = |harness: &mut FrameHarness, what: &str| {
        let repaint = present(harness, &mut frames, key);
        let input = harness.frame_render_input();
        let (regions, full_redraw) = match &repaint {
            Repaint::Full(_) => (&[][..], true),
            Repaint::Regions(regions) => (regions.as_slice(), false),
        };
        raster
            .rasterize(
                (device, queue),
                &mut renderer,
                target,
                FrameContent {
                    scene: input.scene,
                    width: SIZE.0,
                    height: SIZE.1,
                    base_color: input.background,
                    damage_regions: regions,
                    scroll_copies: &[],
                    full_redraw,
                    backdrop: &crate::backdrop::BackdropFrame::default(),
                },
            )
            .0
            .unwrap();
        if full_redraw {
            return;
        }
        partial += 1;
        let check = RasterCheck::start(
            (device, queue),
            &mut renderer,
            target,
            input.scene,
            SIZE,
            input.background,
        )
        .unwrap();
        // Another test polling the shared device may be the one that runs
        // the readback's callbacks, after this poll returns.
        let differing = loop {
            let _ = device.poll(vello::wgpu::PollType::wait_indefinitely());
            if let Some(differing) = check.differing() {
                break differing;
            }
            std::thread::yield_now();
        };
        assert_eq!(
            differing, 0,
            "after {what} (sparse {sparse}), repainting {regions:?} left stale pixels"
        );
    };
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
        partial >= 3,
        "the session repainted in part {partial} times"
    );
}
