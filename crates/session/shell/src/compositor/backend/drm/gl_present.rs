//! The CPU renderer's output presented through the GLES compositor.
//!
//! A lone output whose desktop the CPU renders ([`super::cpu_present`]) is
//! composited with GLES ([`super::gl_compose`]) into GBM scanout buffers
//! when the device's render node brings GLES up, instead of being copied
//! into dumb buffers with the pointer drawn by the CPU. The compositor owns a
//! `!Send` renderer, so it lives on the presentation thread that made it.
//! Where GLES cannot start, or a composite fails, the output goes back to
//! the dumb-buffer copies for the rest of the session.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use tilo_cpu_raster::Region;
use vello::FineDamageRect;

use super::cpu_layers::CpuLayers;
use super::cpu_present::CpuRenderer;
use super::gl_compose::{GlCompositor, LayerDraw, LayerKey, Placement};
use super::runtime::ScanoutEntry;
use super::scanout::{DirectScanoutTargets, DirectSubmit, ScanoutFormat};
use crate::shell::frame::FrameCache;

/// Environment switch: `0` keeps the dumb-buffer copies.
pub const GL_COMPOSE_ENV: &str = "TILO_DRM_GL_COMPOSE";

enum State {
    /// Not tried yet on this thread.
    Untried,
    Ready(Box<GlPresenter>),
    /// GLES cannot present here; the reason was logged once.
    Off,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Untried) };
}

struct GlPresenter {
    compositor: GlCompositor,
    targets: Option<Targets>,
    /// The desktop's layers, while frames come as layers.
    layers: Option<CpuLayers>,
    /// What the last composite drew: the layers (with the strip count) or
    /// the screen canvas.
    shown: Option<Shown>,
    /// The pointer as the last composite drew it: its position and the
    /// address of its image.
    cursor: Option<(i32, i32, usize)>,
    timings: Timings,
    /// The screen canvas's present revision the screen texture was last
    /// brought up to.
    screen_revision: Option<u64>,
}

struct Targets {
    slots: DirectScanoutTargets<Dmabuf>,
    /// The output they were made for: its CRTC and mode size.
    crtc: u32,
    size: (u32, u32),
}

/// What a composite drew.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Screen,
    /// The layers, with where each strip showed.
    Layers(Vec<crate::shell::frame::StripBlit>),
}

/// What a frame presents.
#[derive(Clone, Copy)]
pub enum Content<'a> {
    /// The CPU renderer's screen canvas.
    Screen,
    /// The desktop as layers.
    Layers(&'a FrameCache),
}

fn off(reason: &str) -> State {
    eprintln!("tilo compositor: GLES composition off: {reason}");
    State::Off
}

fn open() -> Result<(GlCompositor, &'static std::path::Path), String> {
    if std::env::var(GL_COMPOSE_ENV).is_ok_and(|value| value == "0") {
        return Err(format!("{GL_COMPOSE_ENV}=0"));
    }
    let node = crate::compositor::render_node::resolved().ok_or("no render node")?;
    Ok((GlCompositor::new(node)?, node))
}

/// Whether GLES composites the CPU renderer's output on this device: asked
/// once, before frames are drawn, so the frames can come as layers.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| match open() {
        Ok(_) => true,
        Err(error) => {
            eprintln!("tilo compositor: frames do not come as layers: {error}");
            false
        }
    })
}

fn start() -> State {
    match open() {
        Ok((compositor, node)) => {
            eprintln!(
                "tilo compositor: GLES composition on {} ({compositor:?})",
                node.display()
            );
            State::Ready(Box::new(GlPresenter {
                compositor,
                targets: None,
                layers: None,
                shown: None,
                cursor: None,
                timings: Timings::default(),
                screen_revision: None,
            }))
        }
        Err(error) => off(&error),
    }
}

/// Present `renderer`'s screen on `entry` through GLES. `false` when GLES
/// does not present here, and the caller copies into dumb buffers instead.
pub fn present(
    entry: &mut ScanoutEntry,
    renderer: &mut CpuRenderer,
    content: Content<'_>,
    pointer: (f64, f64),
    flip_timeout: Duration,
) -> bool {
    STATE.with_borrow_mut(|state| {
        if matches!(state, State::Untried) {
            *state = start();
        }
        let State::Ready(presenter) = state else {
            return false;
        };
        match presenter.present(entry, renderer, content, pointer, flip_timeout) {
            Ok(()) => true,
            Err(error) => {
                // The CRTC may show one of our buffers: the dumb front takes
                // it back, and the whole screen is copied there again.
                entry.scanout.restore();
                renderer.invalidate_screen();
                *state = off(&format!("composite failed: {error}"));
                false
            }
        }
    })
}

impl GlPresenter {
    fn targets(&mut self, entry: &ScanoutEntry) -> Result<&mut Targets, String> {
        let crtc = u32::from(entry.scanout.crtc());
        let size = entry.scanout.size();
        if self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.crtc != crtc || targets.size != size)
        {
            // Targets of another mode or CRTC are not on screen any more.
            self.targets = None;
            let targets = DirectScanoutTargets::build(
                &entry.scanout,
                ScanoutFormat::Xrgb8888,
                size,
                |slot| GlCompositor::scanout_target(&slot),
            )?;
            self.targets = Some(Targets {
                slots: targets,
                crtc,
                size,
            });
            self.cursor = None;
        }
        self.targets
            .as_mut()
            .ok_or_else(|| "no scanout targets".to_owned())
    }

    fn present(
        &mut self,
        entry: &mut ScanoutEntry,
        renderer: &mut CpuRenderer,
        content: Content<'_>,
        pointer: (f64, f64),
        flip_timeout: Duration,
    ) -> Result<(), String> {
        let began = Instant::now();
        let size = entry.scanout.size();
        let shown = match content {
            Content::Screen => Shown::Screen,
            Content::Layers(cache) => {
                let threads = renderer.threads();
                let layers = self.layers.get_or_insert_with(|| CpuLayers::new(threads));
                layers.update(cache, size, renderer.stats_mut());
                renderer.stats_mut().fill = "layers";
                Shown::Layers(cache.strips.clone())
            }
        };
        let cursor = software_pointer(entry, pointer);
        renderer.record_cursor(cursor.is_some());
        let cursor_key = cursor
            .as_ref()
            .map(|(bitmap, x, y)| (*x, *y, std::sync::Arc::as_ptr(bitmap) as usize));
        let changed = match &shown {
            Shown::Screen => renderer.has_pending(),
            Shown::Layers(_) => {
                renderer.stats().did_work()
                    || self.layers.as_ref().is_some_and(CpuLayers::unuploaded)
            }
        };
        if !changed && cursor_key == self.cursor && self.shown.as_ref() == Some(&shown) {
            return Ok(());
        }
        let updated = began.elapsed();
        let targets = self.targets(entry)?;
        // While the previous flip waits for its vblank, this frame is
        // composited into the buffer neither shown nor queued; it is queued
        // once that flip retires.
        let (slot, target) = if targets.slots.flip_pending() {
            targets.slots.overlap_slot()
        } else {
            targets.slots.render_slot()
        }
        .ok_or_else(|| "no free scanout buffer".to_owned())?;
        let mut target = target.clone();
        let full = [Region::new(0, 0, size.0, size.1)];
        // The screen canvas changed only where it has pending damage: while
        // layers were shown it was not drawn into, and its texture still
        // holds it as it was. Unless the dumb buffers showed it meanwhile
        // (another output joined): what they took is not pending any more.
        let written: Vec<Region> = match renderer.pending() {
            Some(rects) if self.screen_revision == Some(renderer.presented_revision()) => rects
                .iter()
                .map(|rect| Region::new(rect.x, rect.y, rect.width, rect.height))
                .collect(),
            _ => full.to_vec(),
        };
        let draws: Vec<LayerDraw<'_>> = match (&shown, &self.layers) {
            (Shown::Layers(_), Some(layers)) => layers.draws(),
            _ => vec![LayerDraw {
                key: LayerKey::Screen,
                canvas: renderer.canvas(),
                written: &written,
                placements: vec![Placement {
                    src: FineDamageRect {
                        x: 0,
                        y: 0,
                        width: size.0,
                        height: size.1,
                    },
                    dst: (0, 0),
                }],
            }],
        };
        let started = Instant::now();
        let stats = self.compositor.compose(
            &mut target,
            size,
            &draws,
            cursor.as_ref().map(|(bitmap, x, y)| (bitmap, *x, *y)),
        )?;
        drop(draws);
        let composed = started.elapsed();
        if let Some(layers) = self.layers.as_mut() {
            layers.uploaded();
        }
        let targets = self
            .targets
            .as_mut()
            .ok_or_else(|| "no scanout targets".to_owned())?;
        let waited = Instant::now();
        retire_pending_flip(entry, &mut targets.slots, renderer, flip_timeout)?;
        let waited = waited.elapsed();
        match entry.scanout.submit_direct_target(&mut targets.slots, slot) {
            DirectSubmit::Queued | DirectSubmit::Presented => {}
            DirectSubmit::Busy => return Ok(()),
            DirectSubmit::Failed(error) | DirectSubmit::HdrFailed(_, error) => return Err(error),
        }
        self.cursor = cursor_key;
        renderer.record_present(started.elapsed(), stats.upload_px, stats.draws);
        if shown == Shown::Screen {
            let revision = renderer.presented_revision().wrapping_add(1);
            renderer.finish_present(revision, true);
            self.screen_revision = Some(revision);
        }
        self.shown = Some(shown);
        self.timings
            .note([updated, composed, waited, began.elapsed()]);
        Ok(())
    }
}

/// The pointer's image and top-left corner, where the compositor draws it:
/// on an output without a cursor plane.
fn software_pointer(
    entry: &ScanoutEntry,
    pointer: (f64, f64),
) -> Option<(
    std::sync::Arc<crate::compositor::cursor::CursorBitmap>,
    i32,
    i32,
)> {
    if entry.scanout.hardware_cursor_active() {
        return None;
    }
    entry.scanout.cursor_bitmap().map(|bitmap| {
        let x = ui::num::i32_of_f64(pointer.0)
            .saturating_sub(i32::try_from(bitmap.hotspot.0).unwrap_or(0));
        let y = ui::num::i32_of_f64(pointer.1)
            .saturating_sub(i32::try_from(bitmap.hotspot.1).unwrap_or(0));
        (bitmap, x, y)
    })
}

/// Wait for the queued flip, if one is, and free what it replaced.
fn retire_pending_flip(
    entry: &mut ScanoutEntry,
    slots: &mut DirectScanoutTargets<Dmabuf>,
    renderer: &mut CpuRenderer,
    timeout: Duration,
) -> Result<(), String> {
    if !slots.flip_pending() {
        return Ok(());
    }
    match entry.scanout.wait_cpu_flip(timeout) {
        Ok((wait, latency)) => {
            renderer.record_flip_wait(wait, latency);
            slots.retire_flip();
            Ok(())
        }
        Err(wait) => {
            renderer.record_flip_wait(wait, None);
            Err(format!(
                "the flip did not retire within {} ms",
                wait.as_millis()
            ))
        }
    }
}

/// Where the composited frames' time went, said every few seconds in the
/// session log: updating the canvases, compositing, waiting for the previous
/// flip, and all of it.
#[derive(Default)]
struct Timings {
    frames: Vec<[Duration; 4]>,
    since: Option<Instant>,
}

impl Timings {
    const EVERY: Duration = Duration::from_secs(5);

    fn note(&mut self, frame: [Duration; 4]) {
        self.frames.push(frame);
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() < Self::EVERY {
            return;
        }
        let quantiles = |index: usize| {
            let mut values: Vec<u128> = self
                .frames
                .iter()
                .map(|frame| frame.get(index).map_or(0, Duration::as_micros))
                .collect();
            values.sort_unstable();
            let at = |q: usize| {
                values
                    .get(values.len().saturating_mul(q) / 100)
                    .copied()
                    .unwrap_or(0)
            };
            format!("{}/{}", at(50), at(90))
        };
        eprintln!(
            "GL-PRESENT frames={} update_us={} compose_us={} flip_wait_us={} total_us={} (p50/p90)",
            self.frames.len(),
            quantiles(0),
            quantiles(1),
            quantiles(2),
            quantiles(3),
        );
        self.frames.clear();
        self.since = None;
    }
}
