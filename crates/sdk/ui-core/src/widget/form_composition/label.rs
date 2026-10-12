//! A field label aligned to its first control, independent of supporting rows.
use super::{
    Affine, HashMap, NodeId, ObjId, Parts, Point, PresenceCtx, Rect, Scene, Size, Style, TaffyTree,
    TextRenderer, Theme, Widget, column_style, length, measure_owned, retained_parts,
};

pub(super) struct FieldLabel {
    pub(super) parts: Parts,
    pub(super) top_inset: f32,
}
impl Widget for FieldLabel {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    retained_parts!("FieldLabel");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            align_self: Some(taffy::AlignSelf::Start),
            ..column_style(0.0)
        };
        let node = self.parts.build(style, tree, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        self.parts.children.first_mut().map_or(Size::ZERO, |label| {
            measure_owned(label.as_mut(), available.width, text)
        })
    }
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.padding.top = length(self.top_inset);
        if *current == style {
            return false;
        }
        tree.set_style(node, style).is_ok()
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
