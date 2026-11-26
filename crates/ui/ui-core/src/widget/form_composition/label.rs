//! A field label aligned to its first control, independent of supporting rows: its field sets the
//! inset that centres it on the control's first line.
use super::{
    HashMap, NodeId, Parts, Point, Rect, Size, Style, TaffyTree, TextRenderer, Theme, Widget,
    column_style, measure_owned, retained_parts,
};

pub(super) struct FieldLabel {
    pub(super) parts: Parts,
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
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}
