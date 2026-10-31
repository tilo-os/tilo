//! [`ImageViewer`] as a described component: a picture to pan and zoom.
//!
//! The viewer pans and zooms a decoded picture. A picture from a view is an address, its bytes
//! loaded as they arrive; [`ZoomableImage`] is the widget that stands in the layout until they
//! do, then shows them in an [`ImageViewer`].

use crate::component::{Component, Described};
use crate::pictures::{Pictures, Shown};
use crate::widget::Widget;

use super::{ImageViewer, PictureSource};

/// A picture to look at closely: scroll to zoom at the pointer, drag to pan, and a reset to the
/// whole picture. It fills its box; the picture is letterboxed inside it.
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
}

/// The picture a viewer shows, by its address, until its bytes arrive: the box it will fill,
/// then an [`ImageViewer`] of it.
#[derive(Default)]
pub struct ZoomableImage {
    shown: Shown,
    viewer: Option<ImageViewer>,
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
            rect: crate::kurbo::Rect::ZERO,
        }
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
        match &mut self.viewer {
            Some(viewer) => viewer.build_layout(taffy, theme, node_map),
            None => {
                use crate::widget::TaffyTreeExt;
                taffy.widget_leaf(
                    taffy::Style {
                        size: taffy::Size::from_percent(1.0, 1.0),
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        ..taffy::Style::default()
                    },
                    node_map,
                    self,
                )
            }
        }
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
            viewer.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &winit::event::WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        Self::of(pictures, image)
    }

    fn set_props(&mut self, props: ImageViewerProps) -> bool {
        let ImageViewerProps {
            image,
            alt: _,
            pictures,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        if self.shown.set(pictures, image) {
            self.viewer = None;
        }
        true
    }
}
