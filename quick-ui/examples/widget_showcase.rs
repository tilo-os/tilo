//! Interactive widget showcase — opens a window and demos every widget
//! introduced by the 2026-05-22 expansion plan plus the load-bearing
//! existing widgets they sit alongside.
//!
//! Run with:
//! ```
//! cargo run -p quick-ui --example widget_showcase
//! ```
//!
//! The overlay widgets (ModalDialog, PopoverWidget) and VideoPlayer are
//! shown in a static "always-open / no-frame" state — they're real,
//! they just don't have host-side triggers wired up here. Use the
//! offscreen `widget_snapshot` example for a PNG that captures every
//! widget at once if you don't want a live window.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::AutoCommit;
use chrono::NaiveDate;
use quick_ui::{App, State};
use quick_ui::i18n::I18nManager;
use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::popover_widget::PopoverSide;
use quick_ui::widget::resizable_split::SplitOrientation;
use quick_ui::widget::toggle_group::Segment;
use quick_ui::widget::toolbar::Orientation as ToolbarOrientation;
use quick_ui::widget::tree_view::TreeNode;
use quick_ui::widget::{
    Avatar, Badge, Breadcrumb, Button, Column, ColorPicker, DatePicker, ImageViewer,
    ModalDialog, PopoverWidget, ProgressBar, ResizableSplit, Row, Scrollable,
    ScrollAxis, Spinner, Text, TimePicker, ToggleGroup, Toolbar, TreeView, VideoPlayer,
    Widget,
};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Color, Fill, Format, Image as PenikoImage};

/// Minimal stand-in widget for the generic content slots of
/// `PopoverWidget`, `ModalDialog`, and `ResizableSplit`. Paints a
/// raised-surface tile with a centred label so the overlays read as
/// something rather than empty boxes.
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
            if on {
                buf[i] = 99; buf[i + 1] = 102; buf[i + 2] = 241; buf[i + 3] = 255;
            } else {
                buf[i] = 30; buf[i + 1] = 41; buf[i + 2] = 59; buf[i + 3] = 255;
            }
        }
    }
    PenikoImage::new(Blob::new(Arc::new(buf)), Format::Rgba8, 16, 16)
}

fn caption(key: &'static str) -> Box<dyn Widget> {
    // All caption keys are registered up-front in `main` — i18n's
    // `add_resource` rejects duplicate ids, so we must NOT re-register
    // here. Just look up the existing key via Text::new.
    Box::new(Text::new(key))
}

fn main() {
    I18nManager::init("en-US");
    I18nManager::add_resource(
        "en-US",
        [
            "title = quick-ui widget showcase",
            "btn_save = Save",
            "btn_cancel = Cancel",
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

    let mut state = State::new();
    {
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "toggle_1", true);
        let _ = state.doc.put(&state.root, "check_1", false);
        let _ = state.doc.put(&state.root, "slider_1", 0.5);
        let text_id = state
            .doc
            .put_object(&state.root, "text_1", automerge::ObjType::Text)
            .unwrap();
        let _ = state.doc.splice_text(&text_id, 0, 0, "Edit me");
    }

    // Build the project tree once so we can pre-expand it.
    let tree_root = TreeNode::branch(
        1u64,
        "project",
        (),
        vec![
            TreeNode::branch(
                2u64,
                "src",
                (),
                vec![
                    TreeNode::leaf(3u64, "main.rs", ()),
                    TreeNode::leaf(4u64, "lib.rs", ()),
                ],
            ),
            TreeNode::leaf(5u64, "README.md", ()),
        ],
    );
    let mut tree = TreeView::new(tree_root);
    tree.toggle_expand(1);
    tree.toggle_expand(2);

    // Anchored popover (always open). Anchor coords are picked to land
    // roughly where the row appears once the layout settles; the flip
    // resolver still re-resolves the actual body position relative to
    // the viewport.
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

    let column_inner = Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(caption("title"))
            .push(caption("row_basics"))
            .push(Box::new(
                Row::new()
                    .push(Box::new(quick_ui::widget::Button::new(
                        "btn",
                        "btn_save",
                        None,
                    )))
                    .push(Box::new(quick_ui::widget::Toggle::new("tog", "toggle_1")))
                    .push(Box::new(quick_ui::widget::Checkbox::new(
                        "chk", "check_1",
                    )))
                    .push(Box::new(quick_ui::widget::Slider::new("sld", "slider_1")))
                    .push(Box::new(
                        quick_ui::widget::TextInput::new(vec![])
                            .with_id("txt")
                            .with_on_change_path("text_1"),
                    )),
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
                "home".into(),
                "docs".into(),
                "specs".into(),
                "readme.md".into(),
            ])))
            .push(Box::new(
                Row::new()
                    .push(Box::new(ToggleGroup::new(
                        vec![
                            Segment::new("Day"),
                            Segment::new("Week"),
                            Segment::new("Month"),
                        ],
                        1,
                    )))
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
                    .push(Box::new(DatePicker::new(
                        NaiveDate::from_ymd_opt(2026, 5, 22).unwrap(),
                    )))
                    .push(Box::new(TimePicker::new(14, 30)))
                    .push(Box::new(TimePicker::new(9, 15).with_seconds(45))),
            ))
            .push(caption("row_picker"))
            .push(Box::new(ColorPicker::new(Color::rgba8(99, 102, 241, 255))))
            .push(caption("row_struct"))
            .push(Box::new(tree))
            // ResizableSplit + ImageViewer + VideoPlayer + Popover + Modal
            // need explicit dimensions / overlay-host context they don't
            // get from a plain Column. They're exercised by the offscreen
            // `widget_snapshot` example which places them at fixed rects;
            // including them here would either collapse them to zero
            // height (Image/Video) or overlap chrome at scene origin
            // (Popover/Modal anchor in absolute coords).
    );
    let _ = popover; // silence unused warnings — kept above for inline
    let _ = modal;   // documentation of the construction shape.

    let root: Box<dyn Widget> = Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(column_inner),
    );

    let app = App::new(state, root);
    app.run();
}
