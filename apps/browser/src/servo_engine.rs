use crate::host::SharedHost;
use dpi::PhysicalSize;
use euclid::Scale;
use servo::{
    ConsoleLogLevel, DeviceIntPoint, DeviceIntRect, EventLoopWaker, LoadStatus, RenderingContext,
    Servo, ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate,
};
use std::rc::Rc;
use std::sync::Arc;
use url::Url;

pub struct ServoEngine {
    pub servo: Option<Servo>,
    pub webview: Option<WebView>,
    pub context: Rc<SoftwareRenderingContext>,
}

pub struct MyWebViewDelegate {
    pub host: SharedHost,
    pub context: Rc<SoftwareRenderingContext>,
}

impl WebViewDelegate for MyWebViewDelegate {
    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        println!("Servo Console [{:?}]: {}", level, message);
    }

    fn notify_new_frame_ready(&self, _webview: WebView) {
        let size = self.context.size();
        let rect = DeviceIntRect::new(
            DeviceIntPoint::new(0, 0),
            DeviceIntPoint::new(size.width as i32, size.height as i32),
        );

        if let Some(image) = self.context.read_to_image(rect) {
            self.host
                .submit_frame(image.into_raw(), size.width as u32, size.height as u32);
        }
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        println!("Servo: Load status changed to {:?}", status);
        if status == LoadStatus::Complete {
            webview.paint();
        }
    }
}

/// Adapter that turns the host's `wake()` into a Servo `EventLoopWaker`.
#[derive(Clone)]
struct HostWaker(SharedHost);
impl EventLoopWaker for HostWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        self.0.wake();
    }
}

impl ServoEngine {
    /// Build a Servo engine bound to a generic [`BrowserHost`]. The host
    /// receives RGBA frames (via `submit_frame`) and event-loop wakes
    /// (via `wake`). Phase A made this trait-based so the iced and
    /// quick-ui chromes can both drive Servo without code changes here.
    pub fn new(url: String, width: u32, height: u32, scale_factor: f32, host: SharedHost) -> Self {
        let waker_host = Arc::clone(&host);
        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(HostWaker(waker_host)))
            .build();

        let mut prefs = servo::prefs::get().clone();
        prefs.dom_intersection_observer_enabled = true;
        servo::prefs::set(prefs);

        let size = PhysicalSize::new(width, height);
        let context = Rc::new(
            SoftwareRenderingContext::new(size).expect("Failed to create SoftwareRenderingContext"),
        );

        let delegate = Rc::new(MyWebViewDelegate {
            host,
            context: context.clone(),
        });

        let webview = WebViewBuilder::new(&servo, context.clone() as Rc<dyn RenderingContext>)
            .delegate(delegate)
            .url(Url::parse(&url).expect("Invalid URL"))
            .build();

        webview.set_hidpi_scale_factor(Scale::new(scale_factor));
        webview.focus();

        Self {
            servo: Some(servo),
            webview: Some(webview),
            context,
        }
    }

    pub fn resize(&self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let size = PhysicalSize::new(width, height);
        if let Some(wv) = &self.webview {
            wv.resize(size);
        }
    }

    pub fn set_scale_factor(&self, scale_factor: f32) {
        if let Some(wv) = &self.webview {
            wv.set_hidpi_scale_factor(Scale::new(scale_factor));
            wv.paint();
        }
    }
}
