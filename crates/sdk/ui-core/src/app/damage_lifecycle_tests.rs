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
        let node = taffy.leaf_node(taffy::Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
