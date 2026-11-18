//! A view laid out as a frame lays it out, for tests that reach into what
//! it composed.

/// The solved tree of a widget laid out as the root.
pub struct LaidOut {
    pub taffy: taffy::TaffyTree<()>,
    /// The widget's own node.
    pub node: taffy::NodeId,
    /// Every widget of the tree by its node, as `build_layout` mapped them.
    pub nodes: std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
}

/// `widget` built, solved with its measured leaves inside a `width` by
/// `height` root, and applied, measuring text with `text`.
pub fn lay_out_as_root_with_text<W: ui::widget::Widget + ?Sized>(
    widget: &mut W,
    width: f32,
    height: f32,
    theme: &ui::theme::Theme,
    text: &mut ui::text_render::TextRenderer,
) -> LaidOut {
    let mut taffy = taffy::TaffyTree::<()>::new();
    let mut nodes = std::collections::HashMap::new();
    let node = widget.build_layout(&mut taffy, theme, &mut nodes);
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            |known, available, node, _, _| {
                nodes.get(&node).map_or(taffy::Size::ZERO, |&widget| {
                    // SAFETY: `build_layout` just filled `nodes` with the
                    // widgets of the tree being solved, which the caller
                    // holds exclusively for the whole pass.
                    unsafe { (*widget).measure(known, available, text) }
                })
            },
        )
        .expect("the layout solves");
    widget.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
    LaidOut { taffy, node, nodes }
}

/// `widget` laid out and drawn until it stops asking for frames, as a host draws it, and
/// reported as drawn after every frame: the last frame's tree, or the `limit`-th when it never
/// rests.
pub fn draw_until_rest<W: ui::widget::Widget + ?Sized>(
    widget: &mut W,
    width: f32,
    height: f32,
    theme: &ui::theme::Theme,
    text: &mut ui::text_render::TextRenderer,
    limit: usize,
) -> LaidOut {
    let mut laid = lay_out_as_root_with_text(widget, width, height, theme, text);
    for _ in 1..limit {
        ui::widget::draw_child_at(
            widget,
            None,
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            text,
            theme,
            &ui::presence::PresenceCtx::empty(),
        );
        ui::motion::frame_drawn(ui::motion::frame_now());
        if !widget.needs_animation_frame() {
            break;
        }
        laid = lay_out_as_root_with_text(widget, width, height, theme, text);
    }
    laid
}
