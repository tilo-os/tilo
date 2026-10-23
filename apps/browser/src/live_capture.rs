//! `--shots-live` and `--shots-live-pip`: the real Chromium engine over the
//! real network, captured without a window. They cover the engine → frame →
//! widget → offscreen-render pipeline that deterministic scenarios stand in
//! for with the test engine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::webview::{FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

use crate::BrowserWidget;
use ui::lock::MutexRecover as _;

/// The page a live capture opens unless `TILO_BROWSER_LIVE_URL` names one.
const DEFAULT_LIVE_URL: &str = "https://example.org";
const LIVE_VIEWPORT: (u32, u32) = (1200, 720);

pub(crate) fn shots_live(out: &str) -> Result<(), String> {
    capture(out, false)
}

/// Capture the live page at `out` with Picture in Picture probed, writing
/// the floating surface it asks for beside it.
pub(crate) fn shots_live_pip(out: &str) -> Result<(), String> {
    capture(out, true)
}

fn capture(out: &str, probe_pip: bool) -> Result<(), String> {
    let url = std::env::var("TILO_BROWSER_LIVE_URL").unwrap_or_else(|_| DEFAULT_LIVE_URL.into());
    render_live_shot_to_png(&url, LIVE_VIEWPORT, std::path::Path::new(out), probe_pip)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Default, serde::Serialize)]
pub(crate) struct PageState {
    uri: String,
    title: String,
    /// The engine reported load completion. This is not an HTTP status check;
    /// captures of error documents still require visual inspection.
    loaded: bool,
    error: Option<String>,
}

pub(crate) struct LiveProbe {
    inner: SharedEngine,
    state: Mutex<PageState>,
}
impl LiveProbe {
    pub(crate) fn new(inner: SharedEngine) -> Arc<Self> {
        Arc::new(Self {
            inner,
            state: Mutex::new(PageState::default()),
        })
    }
    pub(crate) fn state(&self) -> PageState {
        self.state.lock_recover().clone()
    }
}
impl WebViewEngine for LiveProbe {
    fn supports_native_filtering(&self) -> bool {
        self.inner.supports_native_filtering()
    }
    fn send(&self, command: WebViewCommand) {
        self.inner.send(command);
    }
    fn frames(&self) -> FrameSlot {
        self.inner.frames()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.inner.set_waker(waker);
    }
    fn set_visible(&self, visible: bool) {
        self.inner.set_visible(visible);
    }
    fn presented(&self) {
        self.inner.presented();
    }
    fn paint_frame(&self) -> Option<Arc<ui::webview::paint::stream::Frame>> {
        self.inner.paint_frame()
    }
    fn paint_blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.inner.paint_blob(hash)
    }
    fn take_paint_stream(&self, keyframe: bool) -> Vec<ui::webview::paint::stream::wire::Payload> {
        self.inner.take_paint_stream(keyframe)
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let events = self.inner.drain_events();
        let mut state = self.state.lock_recover();
        for event in &events {
            match event {
                WebViewEvent::UriChanged(uri) => {
                    state.uri.clone_from(uri);
                    state.loaded = false;
                    state.error = None;
                }
                WebViewEvent::TitleChanged(title) => state.title.clone_from(title),
                WebViewEvent::LoadFinished => state.loaded = true,
                WebViewEvent::LoadFinishedAt { uri } if uri == &state.uri => state.loaded = true,
                WebViewEvent::LoadFailed { error, .. } => {
                    state.error = Some(error.clone());
                    state.loaded = false;
                }
                _ => {}
            }
        }
        events
    }
}

/// `--shots-live`: real engine, real network. Boot Chromium, navigate, wait
/// for the painted page at the web view's laid-out size, then render
/// one frame to PNG — the same five-pass cycle as `render_shot_to_png`,
/// with an engine-pumping wait between layout and draw.
fn render_live_shot_to_png(
    url: &str,
    viewport: (u32, u32),
    out: &std::path::Path,
    probe_pip: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use ui::time::{Duration, Instant};

    let real_engine: SharedEngine = ui::webview::chromium::ChromiumEngine::new()
        .map_err(|e| format!("Chromium engine failed to boot: {e}"))?;
    let probe = LiveProbe::new(real_engine);
    let engine: SharedEngine = probe.clone();
    // Keep capture chrome/session data isolated while using real maintained
    // protection and the same WebViewEngine event/render pipeline as the app.
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    engine.send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
        crate::blocker::user_scripts(),
    )));
    crate::blocker::spawn_filter_loader(
        engine.clone(),
        Some(WebViewCommand::Navigate(url.to_string())),
    );

    let (w, h) = viewport;
    let theme = ui::launch::shot_theme_from_env()?;
    let presence = PresenceCtx::empty();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);

    let state = ui::State::new();
    let root_obj = state.root.clone();

    // Pump sync + layout until the engine has painted a frame at the
    // web view's laid-out (physical) size — the first layout pass sends
    // the Resize, subsequent frames arrive at the right dimensions.
    // Then let the page settle briefly so late-loading content paints.
    let deadline = ui::time::now() + Duration::from_secs(45);
    let mut sized_frame_seen_at: Option<Instant> = None;
    let mut pip_probe_started = false;
    let mut pip_request = None;
    loop {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        widget.sync(&state.doc, &root_obj);
        let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
        widget.sync(&state.doc, &root_obj);
        let root = taffy.new_with_children(
            Style {
                size: Size::from_lengths(w as f32, h as f32),
                ..Default::default()
            },
            &[child],
        )?;
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: this capture exclusively owns the widget and the exact live
        // layout tree; neither changes between admission and measurement.
        unsafe {
            bindings.rebuild(&widget, &taffy, root, &node_map);
            ui::widget::compute_widget_layout(
                &mut taffy,
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                &node_map,
                &bindings,
                &mut text_renderer,
            )?;
        }
        widget.apply_layout(&taffy, child, Point::ORIGIN);

        if pip_probe_started && pip_request.is_none() {
            pip_request =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut widget);
        }

        let page = probe.state();
        if let Some(error) = &page.error {
            return Err(format!("{}: {error}", page.uri).into());
        }
        let frame = engine
            .frames()
            .lock()
            .map_err(|_| "Capture frame store is unavailable")?
            .clone();
        if let Some(f) = frame {
            let painted = f
                .rgba
                .chunks_exact(4)
                .any(|px| px.last().is_some_and(|alpha| *alpha != 0));
            // Wait for a painted frame at the requested viewport width
            // (edge-to-edge web view, scale 1.0 → frame width ≈ w), so
            // the layout-driven Resize has taken hold rather than the
            // engine's 1024×768 default. Works for any width, wide or
            // narrow.
            let sized = (f.width as i32 - w as i32).abs() <= 2;
            if painted && sized && page.loaded && page.uri.starts_with("http") {
                let seen = *sized_frame_seen_at.get_or_insert_with(Instant::now);
                // Settle long enough that a UA-driven reload (the mobile/
                // desktop switch fires one on the first resize) completes
                // over the network before we capture.
                if seen.elapsed() > Duration::from_secs(6) {
                    if probe_pip && !pip_probe_started {
                        widget.debug_request_picture_in_picture();
                        pip_probe_started = true;
                        continue;
                    }
                    if probe_pip && pip_request.is_none() {
                        continue;
                    }
                    let mut scene = Scene::new();
                    widget.draw(
                        &mut scene,
                        Affine::IDENTITY,
                        &mut text_renderer,
                        &theme,
                        &presence,
                    );
                    return write_live_capture(
                        &scene,
                        viewport,
                        out,
                        pip_request.as_ref(),
                        &page,
                        &theme,
                    );
                }
            }
        }
        if ui::time::now() > deadline {
            return Err(format!("no sized painted frame from {url} within 45s").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_live_capture(
    scene: &Scene,
    viewport: (u32, u32),
    out: &std::path::Path,
    pip_request: Option<&ui::floating_surface::FloatingSurfaceRequest>,
    page: &PageState,
    theme: &ui::theme::Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(scene, theme.tokens.colors.surfaces.content, out);
    if let Some(request) = pip_request {
        let ui::floating_surface::FloatingSource::AppElement {
            element_id, region, ..
        } = &request.source
        else {
            return Err("real browser PiP did not preserve a semantic media element".into());
        };
        if element_id.trim().is_empty() || region.area() <= 1.0 {
            return Err("real browser PiP returned empty semantic geometry".into());
        }
        let protocol: ui::floating_surface::FloatingSurfaceProtocolRequest = request.into();
        let sidecar = std::path::PathBuf::from(format!("{}.pip.json", out.display()));
        std::fs::write(&sidecar, serde_json::to_vec_pretty(&protocol)?)?;
        println!(
            "browser: live PiP selected {} at {:.0} by {:.0}; wrote {}",
            request.title,
            region.width(),
            region.height(),
            sidecar.display()
        );
    }
    std::fs::write(
        format!("{}.page.json", out.display()),
        serde_json::to_vec_pretty(page)?,
    )?;
    println!(
        "browser: wrote live Chromium shot of {} to {}",
        page.uri,
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observing_events_preserves_visible_failures() {
        let engine = ui::webview::chromium::UnavailableEngine::new("missing runtime".into());
        let probe = LiveProbe::new(engine);
        probe.send(WebViewCommand::Navigate("https://example.org".into()));
        let events = probe.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(probe.state().error.as_deref(), Some("missing runtime"));
        assert!(!probe.state().loaded);
        assert!(probe.frames().lock().unwrap().is_none());
    }
}
