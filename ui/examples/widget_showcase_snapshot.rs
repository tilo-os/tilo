//! Renders the `widget_showcase` Column tree to PNG via OffscreenRenderer.
//! Lets reviewers (and Claude) inspect the live App layout without opening
//! a window. Run with:
//! ```
//! cargo run -p quick-ui --example widget_showcase_snapshot
//! ```
//! PNG written to `/tmp/quick-ui-widget-showcase.png`.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::AutoCommit;
use chrono::NaiveDate;
use ui::i18n::I18nManager;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::popover_widget::PopoverSide;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::toggle_group::Segment;
use ui::widget::toolbar::Orientation as ToolbarOrientation;
use ui::widget::tree_view::TreeNode;
use ui::widget::{
    Avatar, Badge, Breadcrumb, Column, ColorPicker, DatePicker, ImageViewer, ModalDialog,
    PopoverWidget, ProgressBar, ResizableSplit, Row, ScrollAxis, Scrollable, Spinner, Text,
    TimePicker, ToggleGroup, Toolbar, TreeView, VideoPlayer, Widget,
};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Color, Fill, Format, Image as PenikoImage};

struct LabelTile {
    text: String,
    rect: Rect,
}
impl LabelTile {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), rect: Rect::ZERO }
    }
}
impl Widget for LabelTile {
    fn sync(&mut self, _: &mut AutoCommit, _: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(taffy::Style {
                size: Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        _available: Size<AvailableSpace>,
        tr: &mut TextRenderer,
    ) -> Size<f32> {
        let (w, h) = tr.measure(&self.text, 14.0, None);
        Size { width: w as f32 + 16.0, height: h as f32 + 12.0 }
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );
        tr.draw_text(
            scene,
            &self.text,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + 8.0, self.rect.y0 + 6.0)),
            None,
        );
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &automerge::ObjId,
        _: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &str { "LabelTile" }
    fn get_rect(&self) -> Rect { self.rect }
}

fn placeholder_image() -> PenikoImage {
    let mut buf = vec![0u8; 4 * 16 * 16];
    for y in 0..16 {
        for x in 0..16 {
            let i = (y * 16 + x) * 4;
            let on = ((x / 4) + (y / 4)) % 2 == 0;
            if on { buf[i] = 99; buf[i + 1] = 102; buf[i + 2] = 241; buf[i + 3] = 255; }
            else  { buf[i] = 30; buf[i + 1] = 41;  buf[i + 2] = 59;  buf[i + 3] = 255; }
        }
    }
    PenikoImage::new(Blob::new(Arc::new(buf)), Format::Rgba8, 16, 16)
}

fn caption(key: &'static str) -> Box<dyn Widget> { Box::new(Text::new(key)) }

fn main() {
    let out_top = std::env::var("QUICK_UI_SHOWCASE_OUT")
        .unwrap_or_else(|_| "/tmp/quick-ui-widget-showcase.png".to_string());
    let out_scrolled = "/tmp/quick-ui-widget-showcase-scrolled.png";
    // Window-sized canvas so we can verify scrolling against a fixed
    // viewport: only ~800 px of content fits at a time, so scrolling
    // 400 px down must reveal new widgets that weren't in the top frame.
    let (canvas_w, canvas_h) = (1100u32, 600u32);

    I18nManager::init("en-US");
    I18nManager::add_resource(
        "en-US",
        [
            "title = quick-ui widget showcase",
            "btn_save = Save",
            "row_basics = Existing widgets (Button, Toggle, Checkbox, Slider, TextInput)",
            "row_indicators = New: indicators",
            "row_avatar_badge = New: avatars + badges",
            "row_nav = New: breadcrumb + toggle group + toolbar",
            "row_datetime = New: date / time pickers",
            "row_picker = New: color picker",
            "row_struct = New: tree view + resizable split",
            "row_media = New: image viewer + video player",
            "row_overlay = New: popover (anchored, open)",
            "row_modal = New: modal (open)",
        ]
        .join("\n"),
    );

    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let mut doc = AutoCommit::new();
    {
        use automerge::transaction::Transactable;
        let _ = doc.put(&automerge::ObjId::Root, "toggle_1", true);
        let _ = doc.put(&automerge::ObjId::Root, "check_1", false);
        let _ = doc.put(&automerge::ObjId::Root, "slider_1", 0.5);
        let text_id = doc
            .put_object(&automerge::ObjId::Root, "text_1", automerge::ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&text_id, 0, 0, "Edit me");
    }

    let tree_root = TreeNode::branch(
        1u64, "project", (),
        vec![
            TreeNode::branch(2u64, "src", (), vec![
                TreeNode::leaf(3u64, "main.rs", ()),
                TreeNode::leaf(4u64, "lib.rs", ()),
            ]),
            TreeNode::leaf(5u64, "README.md", ()),
        ],
    );
    let mut tree = TreeView::new(tree_root);
    tree.toggle_expand(1);
    tree.toggle_expand(2);

    let popover = PopoverWidget::new(
        Rect::new(60.0, 60.0, 220.0, 90.0),
        LabelTile::new("Popover content (always open)"),
        PopoverSide::Below,
    )
    .open(true);

    let modal = ModalDialog::new(
        "Save your changes?",
        LabelTile::new("This is the modal body."),
    )
    .open(true)
    .primary_action("Save", || {})
    .secondary_action("Discard", || {});

    let mut root: Box<dyn Widget> = Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(caption("title"))
            .push(caption("row_basics"))
            .push(Box::new(
                Row::new()
                    .push(Box::new(ui::widget::Button::new("btn", "btn_save", None)))
                    .push(Box::new(ui::widget::Toggle::new("tog", "toggle_1")))
                    .push(Box::new(ui::widget::Checkbox::new("chk", "check_1")))
                    .push(Box::new(ui::widget::Slider::new("sld", "slider_1")))
                    .push(Box::new(ui::widget::TextInput::new(vec![])
                        .with_id("txt").with_on_change_path("text_1"))),
            ))
            .push(caption("row_indicators"))
            .push(Box::new(
                Row::new()
                    .push(Box::new(ProgressBar::new(0.5)))
                    .push(Box::new(ProgressBar::indeterminate()))
                    .push(Box::new(Spinner::with_size(32.0))),
            ))
            .push(caption("row_avatar_badge"))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Avatar::from_initials("QD", 48.0)))
                    .push(Box::new(Avatar::from_initials("AL", 48.0)))
                    .push(Box::new(Avatar::new(placeholder_image(), 48.0)))
                    .push(Box::new(Badge::dot()))
                    .push(Box::new(Badge::count(7)))
                    .push(Box::new(Badge::count(120))),
            ))
            .push(caption("row_nav"))
            .push(Box::new(Breadcrumb::new(vec![
                "home".into(), "docs".into(), "specs".into(), "readme.md".into(),
            ])))
            .push(Box::new(
                Row::new()
                    .push(Box::new(ToggleGroup::new(
                        vec![Segment::new("Day"), Segment::new("Week"), Segment::new("Month")], 1)))
                    .push(Box::new(
                        Toolbar::new(ToolbarOrientation::Horizontal)
                            .push_button("Cut", || {})
                            .push_separator()
                            .push_button("Copy", || {})
                            .push_button("Paste", || {}),
                    )),
            ))
            .push(caption("row_datetime"))
            .push(Box::new(
                Row::new()
                    .push(Box::new(DatePicker::new(NaiveDate::from_ymd_opt(2026, 5, 22).unwrap())))
                    .push(Box::new(TimePicker::new(14, 30)))
                    .push(Box::new(TimePicker::new(9, 15).with_seconds(45))),
            ))
            .push(caption("row_picker"))
            .push(Box::new(ColorPicker::new(Color::from_rgba8(99, 102, 241, 255))))
            .push(caption("row_struct"))
            .push(Box::new(tree))
            // ImageViewer/VideoPlayer/ResizableSplit/Popover/Modal omitted
            // — see `widget_showcase.rs` for the rationale (overlay
            // anchoring + percent-height widgets don't compose into a
            // bare Column). The offscreen `widget_snapshot` example
            // pins them at fixed rects for visual review.
        ))  // close Column → Box::new
    );  // close Scrollable.with_child → Box::new (outer root)
    let _ = popover;
    let _ = modal;


    // Stamp the column at (0, 0) with the canvas size — exactly what the
    // App harness does, just offscreen.
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
    let _ = taffy.compute_layout_with_measure(
        root_node,
        Size {
            width: AvailableSpace::Definite(canvas_w as f32),
            height: AvailableSpace::Definite(canvas_h as f32),
        },
        |known, available, node_id, _, _| {
            if let Some(&ptr) = node_map.get(&node_id) {
                unsafe { (*ptr).measure(known, available, &mut tr) }
            } else {
                Size::ZERO
            }
        },
    );
    root.apply_layout(&taffy, root_node, Point::ORIGIN);
    root.sync(&mut doc, &automerge::ObjId::Root);
    let presence = PresenceCtx::default();
    root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    renderer.render_png(&scene, theme.tokens.colors.surfaces.base, &out_top);
    println!("wrote top-of-page snapshot to {}", out_top);

    // ----- Now scroll down 600 px and re-render to verify scrolling --
    // Inject a synthetic MouseWheel event that the Scrollable should
    // catch and convert to an `offset_y` update. Then re-apply layout
    // (Scrollable's apply_layout re-positions its child using the new
    // offset) and re-draw.
    use winit::event::{MouseScrollDelta, WindowEvent};
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let modifiers = winit::event::Modifiers::default();
    // LineDelta(0, -30) = scroll down 30 lines (~600 px after Scrollable's
    // 20× line multiplier). Negative y = "scroll down" in winit's model.
    let wheel = WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta: MouseScrollDelta::LineDelta(0.0, -30.0),
        phase: winit::event::TouchPhase::Moved,
    };
    root.handle_event(
        &wheel, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );
    // Re-apply layout — the Scrollable child is positioned with
    // (rect.x0 - offset_x, rect.y0 - offset_y), so updated offset means
    // updated child rects.
    root.apply_layout(&taffy, root_node, Point::ORIGIN);
    let mut scene2 = Scene::new();
    root.draw(&mut scene2, Affine::IDENTITY, &mut tr, &theme, &presence);
    renderer.render_png(&scene2, theme.tokens.colors.surfaces.base, out_scrolled);
    println!("wrote scrolled snapshot to {}", out_scrolled);
}
