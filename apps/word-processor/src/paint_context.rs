use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::Affine;

/// Mutable renderer state shared by Writer's immediate-mode auxiliary layers.
///
/// Keeping the scene, transform, and text renderer together prevents every
/// overlay helper from inventing its own three-argument paint convention.
pub(crate) struct PaintContext<'a> {
    pub(crate) scene: &'a mut Scene,
    pub(crate) transform: Affine,
    pub(crate) text: &'a mut TextRenderer,
}

impl<'a> PaintContext<'a> {
    pub(crate) fn new(scene: &'a mut Scene, transform: Affine, text: &'a mut TextRenderer) -> Self {
        Self {
            scene,
            transform,
            text,
        }
    }
}
