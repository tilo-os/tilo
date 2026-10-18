//! Rendering the window previews the switcher cannot draw live.
//!
//! A client window is never rendered here: chrome draws it straight from
//! its buffer in the frame store
//! ([`crate::shell::window_previews::paint_client_frame`]), so its preview
//! is there in the first frame. The overview also paints shell-drawn
//! windows live. What is left is a shell-drawn window on an Alt+Tab card:
//! the switcher asks through
//! [`crate::shell::overlay_host::ShellOverlayHost::window_preview_requests`],
//! which owns the throttle, the turn order and the memory bound, and this
//! paints the window through the same live path a screenshot of it takes
//! ([`FrameBuilder::paint_capture_targets`]), crops it to its window, and
//! renders it into one small offscreen GPU target that exists only while
//! previews are wanted.
//!
//! The rendering never runs on the compositor thread: a render waits on a
//! GPU submission and its readback, which on a busy or software GPU
//! outlasted a frame and stalled the whole desktop. The compositor thread
//! builds the window's scene (that needs its live widgets); a worker thread
//! of its own renders, reads back and hands the pixels back through a
//! channel, waking the loop to collect them. The worker owns the offscreen
//! renderer: wgpu's device and queue are thread-safe, and the renderer is
//! created, used and dropped on that one thread.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Affine;

use crate::compositor::frame::FrameBuilder;
use crate::shell::window_previews::{
    MAX_PREVIEW_HEIGHT, MAX_PREVIEW_WIDTH, RENDERS_PER_REFRESH, preview_size,
};
use tilo_desktop::window_switcher::PreviewSource;

/// What one refresh did, for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewRefresh {
    /// Requests the round went through.
    pub visited: usize,
    /// Previews handed to the render thread this round.
    pub rendered: usize,
    /// Finished previews collected from the render thread this round.
    pub collected: usize,
}

enum Message {
    /// A shell-painted window, already cropped and scaled into its preview.
    Render {
        tab: u64,
        scene: Box<Scene>,
        size: (u32, u32),
    },
    /// No chrome wants previews: drop the GPU target.
    Release,
    /// Answered once every message before it has been handled.
    Fence(mpsc::Sender<()>),
}

/// A finished preview, on its way back to the compositor thread.
struct Rendered {
    tab: u64,
    /// `None` when nothing could be rendered (the GPU target is
    /// unavailable).
    image: Option<(Arc<[u8]>, u32, u32)>,
}

/// Renders a scene of a preview's size into its top-left corner and reads
/// it back: the GPU in production, a stand-in in tests.
pub trait PreviewBackend: Send {
    /// The top-left `size` of `scene`, read back as tightly packed RGBA.
    fn render(&mut self, scene: &Scene, size: (u32, u32)) -> Option<Vec<u8>>;
    /// Drop whatever GPU target is held.
    fn release(&mut self);
    /// Whether a GPU target is held.
    fn holds_target(&self) -> bool;
    /// Whether rendering can never work here, so nothing more is asked.
    fn unavailable(&self) -> bool;
}

/// The production backend: one offscreen target at the largest preview size,
/// made on first use.
#[derive(Default)]
struct GpuBackend {
    renderer: Option<OffscreenRenderer>,
    unavailable: bool,
}

impl PreviewBackend for GpuBackend {
    fn render(&mut self, scene: &Scene, (width, height): (u32, u32)) -> Option<Vec<u8>> {
        if self.renderer.is_none() && !self.unavailable {
            match OffscreenRenderer::try_new(MAX_PREVIEW_WIDTH, MAX_PREVIEW_HEIGHT) {
                Ok(renderer) => self.renderer = Some(renderer),
                Err(error) => {
                    tracing::warn!(%error, "window previews unavailable: no offscreen target");
                    self.unavailable = true;
                }
            }
        }
        let renderer = self.renderer.as_mut()?;
        let full = renderer.render_rgba_resilient(scene);
        crop_top_left(&full, renderer.width, width, height)
    }

    fn release(&mut self) {
        self.renderer = None;
    }

    fn holds_target(&self) -> bool {
        self.renderer.is_some()
    }

    fn unavailable(&self) -> bool {
        self.unavailable
    }
}

/// The render thread and the two channels to it.
struct Worker {
    messages: mpsc::Sender<Message>,
    finished: mpsc::Receiver<Rendered>,
    /// Mirrors the backend's state for the compositor thread.
    holds_target: Arc<AtomicBool>,
    unavailable: Arc<AtomicBool>,
}

impl Worker {
    fn spawn(mut backend: Box<dyn PreviewBackend>, wake: std::task::Waker) -> Option<Self> {
        let (messages, inbox) = mpsc::channel::<Message>();
        let (outbox, finished) = mpsc::channel::<Rendered>();
        let holds_target = Arc::new(AtomicBool::new(false));
        let unavailable = Arc::new(AtomicBool::new(false));
        let (held, gone) = (Arc::clone(&holds_target), Arc::clone(&unavailable));
        let spawned = std::thread::Builder::new()
            .name("tilo-window-previews".into())
            .spawn(move || {
                // Ends when the compositor drops its sender.
                for message in inbox {
                    match message {
                        Message::Render { tab, scene, size } => {
                            let rendered = Rendered {
                                tab,
                                image: backend
                                    .render(&scene, size)
                                    .map(|rgba| (Arc::from(rgba), size.0, size.1)),
                            };
                            held.store(backend.holds_target(), Ordering::Release);
                            gone.store(backend.unavailable(), Ordering::Release);
                            if outbox.send(rendered).is_err() {
                                break;
                            }
                            wake.wake_by_ref();
                        }
                        Message::Release => {
                            backend.release();
                            held.store(false, Ordering::Release);
                        }
                        Message::Fence(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self {
                messages,
                finished,
                holds_target,
                unavailable,
            }),
            Err(error) => {
                tracing::warn!(%error, "window previews unavailable: no render thread");
                None
            }
        }
    }
}

/// Renders requested window previews on a thread of its own.
pub struct WindowPreviewRenderer {
    /// Made on first use, with the backend `make` builds.
    worker: Option<Worker>,
    make: Option<Box<dyn FnOnce() -> Box<dyn PreviewBackend>>>,
    /// Previews handed to the render thread and not yet back, by tab.
    in_flight: HashSet<u64>,
}

impl Default for WindowPreviewRenderer {
    fn default() -> Self {
        Self::with_backend(|| Box::new(GpuBackend::default()))
    }
}

impl WindowPreviewRenderer {
    /// A renderer whose render thread uses the backend `make` returns.
    pub fn with_backend(make: impl FnOnce() -> Box<dyn PreviewBackend> + 'static) -> Self {
        Self {
            worker: None,
            make: Some(Box::new(make)),
            in_flight: HashSet::new(),
        }
    }

    /// Collect finished previews, then run a refresh round if one is due at
    /// `now`, handing its renders to the render thread. Never waits on the
    /// GPU. Drops the GPU target while no chrome wants previews.
    pub fn refresh(&mut self, frame_builder: &mut FrameBuilder, now: Instant) -> PreviewRefresh {
        let mut report = PreviewRefresh {
            collected: self.collect(frame_builder),
            ..PreviewRefresh::default()
        };
        let requests = frame_builder.host_mut().window_preview_requests(now);
        if requests.is_empty() {
            if frame_builder.host().window_previews().is_empty() {
                self.release();
            }
            return report;
        }
        for request in requests {
            if report.rendered >= RENDERS_PER_REFRESH || self.in_flight.len() >= RENDERS_PER_REFRESH
            {
                break;
            }
            report.visited = report.visited.saturating_add(1);
            // A client window is drawn live from its buffer; only a
            // shell-drawn one is rendered here.
            let queued = match &request.source {
                PreviewSource::Painted { capture_id } => {
                    self.queue_painted(frame_builder, request.tab, capture_id)
                }
                PreviewSource::Client { .. }
                | PreviewSource::Withheld
                | PreviewSource::Unavailable => false,
            };
            if queued {
                report.rendered = report.rendered.saturating_add(1);
            }
        }
        frame_builder
            .host_mut()
            .advance_window_previews(report.visited);
        report
    }

    /// Keep every preview the render thread has finished. Returns how many
    /// came back.
    fn collect(&mut self, frame_builder: &mut FrameBuilder) -> usize {
        let Some(worker) = self.worker.as_ref() else {
            return 0;
        };
        let finished: Vec<Rendered> = worker.finished.try_iter().collect();
        if finished.is_empty() {
            return 0;
        }
        // A preview that comes back after chrome stopped wanting previews is
        // dropped, not kept for a card nobody shows.
        let wanted = frame_builder.host().wants_window_previews();
        let count = finished.len();
        for rendered in finished {
            self.in_flight.remove(&rendered.tab);
            if let (true, Some((rgba, width, height))) = (wanted, rendered.image) {
                frame_builder.host_mut().store_window_preview(
                    rendered.tab,
                    rgba,
                    width,
                    height,
                    None,
                );
            }
        }
        count
    }

    fn queue_painted(
        &mut self,
        frame_builder: &mut FrameBuilder,
        tab: u64,
        capture_id: &str,
    ) -> bool {
        if self.in_flight.contains(&tab) {
            return false;
        }
        // The live repaint needs the shell's widgets, so the scene is built
        // here; only its rendering moves to the render thread.
        let (source, rects) = frame_builder.paint_capture_targets(
            &[capture_id.to_string()],
            ui::widget::CaptureExtent::FullApp,
            false,
        );
        let Some(window) = rects.first().copied().filter(|rect| rect.area() > 0.0) else {
            return false;
        };
        let Some((width, height)) = preview_size(window.width(), window.height()) else {
            return false;
        };
        let transform = Affine::translate((-window.x0, -window.y0)).then_scale_non_uniform(
            f64::from(width) / window.width(),
            f64::from(height) / window.height(),
        );
        let mut scene = Scene::new();
        crate::compositor::capture::append_clipped(&mut scene, &source, window, transform);
        self.send(tab, Box::new(scene), (width, height))
    }

    /// Hand one render to the render thread, starting it on first use.
    fn send(&mut self, tab: u64, scene: Box<Scene>, size: (u32, u32)) -> bool {
        if self.worker.is_none() {
            let Some(make) = self.make.take() else {
                return false;
            };
            self.worker = Worker::spawn(make(), crate::shell::search::invocation_wake::waker());
        }
        let Some(worker) = self.worker.as_ref() else {
            return false;
        };
        if worker.unavailable.load(Ordering::Acquire) {
            return false;
        }
        if worker
            .messages
            .send(Message::Render { tab, scene, size })
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(tab);
        true
    }

    fn release(&mut self) {
        self.in_flight.clear();
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.messages.send(Message::Release);
        }
    }

    /// Wait until the render thread has handled everything handed to it,
    /// then collect what it finished. Blocks, so it is for tests and
    /// shutdown, never the frame loop.
    pub fn settle(&mut self, frame_builder: &mut FrameBuilder) -> usize {
        if let Some(worker) = self.worker.as_ref() {
            let (done, wait) = mpsc::channel();
            if worker.messages.send(Message::Fence(done)).is_ok() {
                let _ = wait.recv();
            }
        }
        self.collect(frame_builder)
    }

    /// Whether the render thread holds a GPU target.
    pub fn holds_gpu_target(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| worker.holds_target.load(Ordering::Acquire))
    }

    /// Previews handed to the render thread and not collected yet.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

/// The top-left `width` × `height` pixels of an RGBA buffer `stride` pixels wide.
fn crop_top_left(rgba: &[u8], stride: u32, width: u32, height: u32) -> Option<Vec<u8>> {
    let stride = usize::try_from(stride).ok()?.checked_mul(4)?;
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = usize::try_from(height).ok()?;
    if row > stride {
        return None;
    }
    let mut out = Vec::with_capacity(row.checked_mul(rows)?);
    for y in 0..rows {
        let start = y.checked_mul(stride)?;
        out.extend_from_slice(rgba.get(start..start.checked_add(row)?)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_keeps_exactly_the_preview_rows() {
        let stride = 4_u32;
        let rgba: Vec<u8> = (0..(stride * 3 * 4))
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let cropped = crop_top_left(&rgba, stride, 2, 2).expect("fits");
        assert_eq!(cropped.len(), 2 * 2 * 4);
        assert_eq!(cropped.get(0..8), rgba.get(0..8));
        assert_eq!(cropped.get(8..16), rgba.get(16..24));
        assert!(crop_top_left(&rgba, stride, 5, 1).is_none());
        assert!(crop_top_left(&rgba, stride, 2, 9).is_none());
    }
}
