//! [`Slideshow`] as a described component: a picture shown large, each shown its time, the next
//! asked for on the frame's clock. What a presentation shows, and how long each picture stays, is
//! the view's; the advance is a frame's, never a change to the workspace.

use super::{ImageViewerProps, PictureSource, ZoomableImage};
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{component::Component, component::Described, component::Event};
use crate::pictures::Pictures;
use crate::time::{Duration, Instant};

/// How long a picture stays when the view names no time: five seconds.
const DEFAULT_SECONDS: f64 = 5.0;
/// The least time a picture stays, however short the view asks: a quarter second.
const LEAST_MILLIS: u64 = 250;

/// A picture shown large in the window for its time, then the next asked for: a presentation a
/// view opens, with the slide it shows kept by the view.
#[derive(Component, Default)]
#[component(name = "Slideshow", role = "Image", label = "alt")]
pub struct SlideshowProps {
    /// The picture on screen now.
    #[component(required, positional)]
    pub image: PictureSource,
    /// What the picture is, as assistive technology announces it.
    #[component(required)]
    pub alt: Option<String>,
    /// How long it stays before the next is asked for, in seconds.
    pub seconds: Option<f64>,
    /// The picture has been shown its time: the view asks for the next.
    #[component(event)]
    pub next: Event,
    /// The window's pictures, through which the bytes load.
    #[component(skip)]
    pub pictures: Option<Pictures>,
}

/// A picture on the frame's clock: it asks for the next once its time is up.
pub struct Slideshow {
    image: ZoomableImage,
    /// The address on screen, so a picture the view changed restarts its time, and the same one
    /// keeps what is left of it.
    address: String,
    interval: Duration,
    started: Instant,
    next: Event,
    asked: bool,
}

impl Slideshow {
    fn restart(&mut self) {
        self.started = tilo_ui_base::motion::frame_now();
        self.asked = false;
    }

    /// Ask for the next picture once this one's time is up, on the frame's clock.
    fn tick(&mut self) {
        let now = tilo_ui_base::motion::frame_now();
        if now >= self.started + self.interval {
            self.started = now;
            self.asked = true;
            self.next.fire(None);
        }
    }
}

impl Widget for Slideshow {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        self.image.sync(state, path);
        self.tick();
    }

    fn update(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) -> bool {
        let moved = self.image.update(state, path);
        self.tick();
        moved || self.asked
    }

    /// It asks for frames while a picture is shown, so its time is kept even when nothing else
    /// moves.
    fn needs_redraw(&self) -> bool {
        self.image.needs_redraw() || !self.next.is_none()
    }

    fn mark_drawn(&mut self) {
        self.asked = false;
        self.image.mark_drawn();
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.image.build_layout(taffy, theme, node_map);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.image.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        tilo_ui_base::widget::draw_child(&mut self.image, scene, transform, text, theme, &env);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.image.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.image.children()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.image.for_each_child(visitor);
    }

    /// The picture is the box: it asks layout exactly when the picture does.
    fn is_layout_dirty(&self) -> bool {
        self.image.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.image.layout_needs_measure()
    }

    fn get_rect(&self) -> crate::kurbo::Rect {
        self.image.get_rect()
    }

    fn name(&self) -> &'static str {
        "Slideshow"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.image.accessibility()
    }
}

impl Described for Slideshow {
    type Props = SlideshowProps;

    fn from_props(key: &str, props: SlideshowProps) -> Self {
        let SlideshowProps {
            image,
            alt,
            seconds,
            next,
            pictures,
        } = props;
        let address = image.src.clone();
        let interval = interval_of(seconds);
        let inner = ZoomableImage::from_props(
            key,
            ImageViewerProps {
                image,
                alt,
                pictures,
                aspect: None,
                zoom: None,
            },
        );
        let mut slideshow = Self {
            image: inner,
            address,
            interval,
            started: tilo_ui_base::motion::frame_now(),
            next,
            asked: false,
        };
        slideshow.restart();
        slideshow
    }

    fn set_props(&mut self, props: SlideshowProps) -> bool {
        let SlideshowProps {
            image,
            alt,
            seconds,
            next,
            pictures,
        } = props;
        self.interval = interval_of(seconds);
        self.next = next;
        let changed = image.src != self.address;
        self.address.clone_from(&image.src);
        let same = self.image.set_props(ImageViewerProps {
            image,
            alt,
            pictures,
            aspect: None,
            zoom: None,
        });
        if changed {
            self.restart();
        }
        same || changed
    }
}

/// The time a picture stays, in whole milliseconds, never shorter than [`LEAST_MILLIS`].
fn interval_of(seconds: Option<f64>) -> Duration {
    let seconds = seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0);
    let millis = seconds.map_or(DEFAULT_SECONDS * 1000.0, |seconds| seconds * 1000.0);
    Duration::from_millis((millis.round().max(0.0) as u64).max(LEAST_MILLIS))
}
