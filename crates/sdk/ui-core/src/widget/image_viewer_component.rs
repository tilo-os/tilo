//! [`ImageViewer`] as a described component: a picture to pan and zoom.

use crate::component::{Component, Described};
use crate::pictures::{Pictures, Shown};
use crate::widget::Widget;

use super::{ImageViewer, PictureSource};
use crate::widget::DrawChild;

/// A picture to look at closely: scroll to zoom at the pointer, drag to pan, and a reset to
/// the whole picture.
#[derive(Component, Default)]
#[component(name = "ImageViewer", role = "Image", label = "alt")]
pub struct ImageViewerProps {
    /// The picture to show.
    #[component(required, positional)]
    pub image: PictureSource,
    /// What the picture is, as assistive technology announces it.
    #[component(required)]
    pub alt: Option<String>,
    /// The window's pictures, through which the bytes load.
    #[component(skip)]
    pub pictures: Option<Pictures>,
    /// The box's shape: its width over its height, which it stands at before the picture is
    /// here.
    pub aspect: Option<f64>,
}

/// The picture a viewer shows, by its address, until its bytes arrive: the box it will
/// fill, then an [`ImageViewer`] of it.
#[derive(Default)]
pub struct ZoomableImage {
    shown: Shown,
    viewer: Option<ImageViewer>,
    /// The width over the height the box stands at before the picture's shape is known: the
    /// picture's own once it is here.
    aspect: Option<f64>,
    rect: crate::kurbo::Rect,
}

impl ZoomableImage {
    /// A viewer of `picture`, loaded through `pictures`.
    #[must_use]
    pub fn of(pictures: Pictures, picture: PictureSource) -> Self {
        let mut shown = Shown::default();
        shown.set(pictures, picture);
        Self {
            shown,
            viewer: None,
            aspect: None,
            rect: crate::kurbo::Rect::ZERO,
        }
    }

    /// The box stands at `width / height` before the picture's shape is known.
    #[must_use]
    pub fn with_aspect(mut self, aspect: f64) -> Self {
        self.aspect = Some(aspect);
        self
    }

    /// Take the picture's bytes once they are here, and show them in a viewer.
    fn take_picture(&mut self) -> bool {
        if !self.shown.resolve() {
            return false;
        }
        let Some(image) = self.shown.image() else {
            return false;
        };
        self.viewer = Some(ImageViewer::new(image.clone()));
        true
    }

    /// The viewer, once the picture is here.
    #[must_use]
    pub fn viewer(&self) -> Option<&ImageViewer> {
        self.viewer.as_ref()
    }

    /// The viewer, once the picture is here.
    pub fn viewer_mut(&mut self) -> Option<&mut ImageViewer> {
        self.viewer.as_mut()
    }
}

impl Widget for ZoomableImage {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.take_picture();
        if let Some(viewer) = &mut self.viewer {
            viewer.sync(state, path);
        }
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let arrived = self.take_picture();
        let forwarded = self
            .viewer
            .as_mut()
            .is_some_and(|viewer| viewer.update(state, path));
        arrived || forwarded || self.shown.waiting()
    }

    fn needs_redraw(&self) -> bool {
        self.shown.waiting()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // One leaf of its own, whether or not the picture is here: the viewer paints into the
        // rect it is given, so it never takes a percentage height of its own (which, inside a
        // scrolling page whose height is its content's, never resolves). The box takes the
        // width it is given and stands at its aspect until the picture's shape is known.
        let _ = theme;
        use crate::widget::TaffyTreeExt;
        let aspect = self.aspect.unwrap_or(4.0 / 3.0);
        taffy.widget_leaf(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                // A box with no shape still stands at 4:3, so a leaf that is not yet sized
                // does not collapse to nothing.
                aspect_ratio: Some(crate::num::f32_of_f64(aspect)),
                flex_shrink: 0.0,
                ..taffy::Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(viewer) = &mut self.viewer {
            viewer.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: crate::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if let Some(viewer) = &mut self.viewer {
            viewer.draw_child(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if let Some(viewer) = &mut self.viewer {
            viewer.handle_event(event, ctx);
        }
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.viewer.as_ref().map_or(self.rect, Widget::get_rect)
    }

    fn name(&self) -> &'static str {
        "ImageViewer"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.viewer.as_ref().and_then(Widget::accessibility)
    }
}

impl Described for ZoomableImage {
    type Props = ImageViewerProps;

    fn from_props(_key: &str, props: ImageViewerProps) -> Self {
        let ImageViewerProps {
            image,
            alt: _,
            pictures,
            aspect,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        let mut zoomable = Self::of(pictures, image);
        if let Some(aspect) = aspect {
            zoomable = zoomable.with_aspect(aspect);
        }
        zoomable
    }

    fn set_props(&mut self, props: ImageViewerProps) -> bool {
        let ImageViewerProps {
            image,
            alt: _,
            pictures,
            aspect,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        self.aspect = aspect;
        if self.shown.set(pictures, image) {
            self.viewer = None;
        }
        true
    }
}
