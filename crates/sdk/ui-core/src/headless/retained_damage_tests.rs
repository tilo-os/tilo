//! A host that keeps the frame it presented repaints only where the next
//! frame changed (`retained_raster`): typing repaints the composer, and
//! anything the parts cannot prove (a resize) repaints the whole frame. The
//! retained target must always equal a full render of the same frame.

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

/// `root` in a frame of [`SIZE`], parts tracked, laid out once as a frame
/// does.
fn desktop_of(root: impl Widget + 'static) -> FrameHarness {
    let mut harness = FrameHarness::for_root(crate::State::new(), root, SIZE, Theme::dark());
    harness.track_scene_parts(true);
    // As the browser does: inputs are laid out and drawn once, by the frame.
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

/// A host's retained target on the shared GPU: each frame repaints only where
/// it changed, what it repaints is culled to the parts that can reach the
/// damage (as the hosts cull it), and every repaint is compared with a full
/// render of the frame.
struct Retained {
    device: vello::wgpu::Device,
    queue: vello::wgpu::Queue,
    renderer: vello::Renderer,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
    raster: RetainedRaster,
    frames: RetainedFrames,
    /// How many frames it repainted in part.
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
            partial: 0,
            sparse,
        }
    }

    /// Repaint what this frame changed onto the target, then compare the
    /// target with a full render of the frame.
    fn step(&mut self, harness: &mut FrameHarness, key: TargetKey, what: &str) {
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
            return;
        }
        self.partial += 1;
        let whole = culled.is_some().then(|| {
            let mut whole = vello::Scene::new();
            whole.append(input.scene, Some(key.to_device));
            whole
        });
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
        )
        .unwrap();
        // Another test polling the shared device may be the one that runs
        // the readback's callbacks, after this poll returns.
        let readback = loop {
            let _ = self.device.poll(vello::wgpu::PollType::wait_indefinitely());
            if let Some(readback) = check.differences() {
                break readback;
            }
            std::thread::yield_now();
        };
        let sparse = self.sparse;
        match readback {
            Readback::Differing(differences) => assert_eq!(
                differences.count, 0,
                "after {what} (sparse {sparse}), repainting {damage:?} left stale pixels at {:?}",
                differences.bounds
            ),
            // A check that could not run proves nothing either way, and the
            // test says so rather than calling the frame stale.
            Readback::Unreadable(why) => panic!(
                "after {what} (sparse {sparse}), the check could not read its frames back: {why}"
            ),
        }
    }
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

/// A desk under an overlay's scrim, as search dims the frame behind its
/// palette.
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

/// The desk of [`desktop`] under a scrim that opens and closes.
fn scrimmed_desktop() -> (FrameHarness, Rc<RefCell<Places>>) {
    let places = Rc::new(RefCell::new(Places::default()));
    let desk = Scrimmed {
        row: desk_row(Rc::clone(&places)),
        open: false,
    };
    (desktop_of(desk), places)
}

/// A scrim dims every pixel of the frame. Closing it must repaint every pixel
/// it dimmed: no frame may keep the dim, not the one that closes it and not
/// the unchanged frames after it.
#[test]
fn closing_a_scrim_repaints_the_frame_it_dimmed() {
    let (mut harness, places) = scrimmed_desktop();
    let key = key(&harness, SIZE, 0);
    let mut target = Retained::new(false);
    target.step(&mut harness, key, "the first frame");

    harness.root_mut::<Scrimmed>().open = true;
    harness.relayout();
    target.step(&mut harness, key, "opening the scrim");

    // What a person does under the scrim: typing repaints its caret and line,
    // and the dim stays as it was over everything else.
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

/// A note: a page that scrolls a block editor of many paragraphs, as a
/// tile's app. The editor is as tall as its paragraphs.
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

/// A note of eight paragraphs in a window, drawn once with its scene parts
/// tracked, and where its editor is.
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

/// A new paragraph at the end of a note grows its editor by a line: the
/// tile repaints that line and the caret, not the whole editor, whose own
/// damage says where its blocks were and are.
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

/// A selection dragged down a note, within a paragraph and then across
/// paragraphs, repaints the rows whose selection changed with each move,
/// not the editor.
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
