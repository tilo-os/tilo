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
    /// How far the picture is zoomed, 1 being the whole picture: what a view's own zoom controls
    /// set. The person's scroll and drag change it too, within the picture, and the view keeps
    /// the value it shows.
    pub zoom: Option<f64>,
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
    /// The zoom the view last set, so a person's scroll and drag inside the picture are not
    /// undone by the next update carrying the same value: only a changed value is applied.
    zoom: Option<f64>,
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
            zoom: None,
            rect: crate::kurbo::Rect::ZERO,
        }
    }

    /// The picture shown at `zoom`, from its first frame.
    #[must_use]
    pub fn with_zoom(mut self, zoom: f64) -> Self {
        self.zoom = Some(zoom);
        self.apply_zoom();
        self
    }

    /// Put the view's zoom on the viewer, once the picture is here.
    fn apply_zoom(&mut self) {
        if let (Some(viewer), Some(zoom)) = (&mut self.viewer, self.zoom) {
            viewer.set_zoom(crate::num::f32_of_f64(zoom));
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
        self.apply_zoom();
        true
    }

    /// The viewer, once the picture is here.
    #[must_use]
    pub fn viewer(&self) -> Option<&ImageViewer> {
        self.viewer.as_ref()
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

    /// One box at the aspect it was built at: placed once, it asks for no layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == crate::kurbo::Rect::ZERO
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

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        if let Some(viewer) = &mut self.viewer {
            viewer.draw_child(scene, transform, text, theme,&env);
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
            zoom,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        let mut zoomable = Self::of(pictures, image);
        if let Some(aspect) = aspect {
            zoomable = zoomable.with_aspect(aspect);
        }
        if let Some(zoom) = zoom {
            zoomable = zoomable.with_zoom(zoom);
        }
        zoomable
    }

    fn set_props(&mut self, props: ImageViewerProps) -> bool {
        let ImageViewerProps {
            image,
            alt: _,
            pictures,
            aspect,
            zoom,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        self.aspect = aspect;
        if self.shown.set(pictures, image) {
            self.viewer = None;
        }
        // The view's controls set the zoom; a value it did not change leaves the person's own
        // scroll and drag alone.
        if zoom != self.zoom {
            self.zoom = zoom;
            self.apply_zoom();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view's zoom controls reach the picture: a viewer opens at the zoom its view set, not
    /// at the whole picture, so a view that keeps a zoom shows it from the first frame.
    #[test]
    fn a_view_opens_the_picture_at_the_zoom_it_set() {
        let png =
            crate::png::encode_rgba8(4, 4, &[40, 80, 160, 255].repeat(16)).expect("a picture");
        let fetch: crate::image_store::Fetcher = std::sync::Arc::new(move |_| Some(png.clone()));
        let pictures = Pictures::new(crate::ImageStore::new(&fetch));
        let props = ImageViewerProps {
            image: PictureSource {
                src: "data:image/png;base64,x".to_owned(),
            },
            alt: None,
            pictures: Some(pictures),
            aspect: None,
            zoom: Some(2.0),
        };
        let mut zoomable = ZoomableImage::from_props("viewer", props);
        let _ = zoomable.take_picture();
        assert_eq!(zoomable.viewer().map(ImageViewer::zoom), Some(2.0));
    }
}
