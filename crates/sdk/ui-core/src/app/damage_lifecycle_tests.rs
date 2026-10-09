use super::*;
use crate::widget::TaffyTreeExt;
use std::collections::HashMap;

struct DrawUpdatedDamage {
    damage: KurboRect,
    dirty: bool,
}

impl Widget for DrawUpdatedDamage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.damage = KurboRect::new(80.0, 0.0, 100.0, 8.0);
    }

    fn name(&self) -> &'static str {
        "DrawUpdatedDamage"
    }

    fn get_rect(&self) -> KurboRect {
        self.damage
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn collect_damage(&self, regions: &mut Vec<KurboRect>) {
        regions.push(self.damage);
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

#[test]
fn submitted_damage_uses_draw_updated_geometry_before_dirty_clear() {
    let mut widget = DrawUpdatedDamage {
        damage: KurboRect::new(0.0, 0.0, 20.0, 8.0),
        dirty: true,
    };
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    widget.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default(),
    );

    let mut regions = Vec::new();
    collect_submitted_widget_damage(&widget, &mut regions);
    assert_eq!(regions, vec![KurboRect::new(80.0, 0.0, 100.0, 8.0)]);
    assert!(widget.dirty, "collection must not consume the dirty flag");

    widget.mark_drawn();
    assert!(!widget.dirty);
}

/// A leaf whose box the test can shift without rebuilding the tree, the way
/// scrolling or a reflowed sibling moves a retained widget.
struct Shiftable {
    shift: std::rc::Rc<std::cell::Cell<f64>>,
    rect: KurboRect,
}

impl Widget for Shiftable {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(120.0),
                height: taffy::Dimension::length(40.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x) + self.shift.get();
        let y = origin.y + f64::from(layout.location.y);
        self.rect = KurboRect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn name(&self) -> &'static str {
        "Shiftable"
    }

    fn get_rect(&self) -> KurboRect {
        self.rect
    }
}

/// A relayout repaints the whole window only when something moved: an
/// in-place change (typing into a fixed-size field) keeps to its damage,
/// while moved geometry or a new viewport cannot be described by it.
#[test]
fn relayout_redraws_everything_only_when_geometry_moves() {
    let shift = std::rc::Rc::new(std::cell::Cell::new(0.0));
    let mut app = App::new(
        State::new(),
        Box::new(Shiftable {
            shift: shift.clone(),
            rect: KurboRect::ZERO,
        }),
    );
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    let (rebuilt, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, true, None).unwrap();
    assert!(rebuilt, "a rebuilt tree repaints everything");

    app.layout.layout_dirty = true;
    let (moved, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, false, None).unwrap();
    assert!(
        !moved,
        "a relayout that moves nothing keeps to widget damage"
    );

    shift.set(24.0);
    app.layout.layout_dirty = true;
    let (moved, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, false, None).unwrap();
    assert!(moved, "a widget that moved leaves its old pixels behind");

    let (resized, ..) = run_layout_pass(&mut app, root, 500, 300, 1.0, false, None).unwrap();
    assert!(resized, "a new viewport repaints everything");

    let (idle, ..) = run_layout_pass(&mut app, root, 500, 300, 1.0, false, None).unwrap();
    assert!(!idle, "no layout work, no full repaint");
}
