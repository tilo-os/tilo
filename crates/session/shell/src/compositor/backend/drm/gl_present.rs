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
}

struct Targets {
    targets: DirectScanoutTargets<Dmabuf>,
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
    *AVAILABLE.get_or_init(|| open().is_ok())
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
                targets,
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
        let size = entry.scanout.size();
        let shown = match content {
            Content::Screen => Shown::Screen,
            Content::Layers(cache) => {
                let threads = renderer.threads();
                let layers = self.layers.get_or_insert_with(|| CpuLayers::new(threads));
                layers.update(cache, size, renderer.stats_mut());
                Shown::Layers(cache.strips.clone())
            }
        };
        let cursor = if entry.scanout.hardware_cursor_active() {
            None
        } else {
            entry.scanout.cursor_bitmap().map(|bitmap| {
                let x = ui::num::i32_of_f64(pointer.0)
                    .saturating_sub(i32::try_from(bitmap.hotspot.0).unwrap_or(0));
                let y = ui::num::i32_of_f64(pointer.1)
                    .saturating_sub(i32::try_from(bitmap.hotspot.1).unwrap_or(0));
                (bitmap, x, y)
            })
        };
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
        let targets = self.targets(entry)?;
        if targets.targets.flip_pending() {
            match entry.scanout.wait_cpu_flip(flip_timeout) {
                Ok((wait, latency)) => {
                    renderer.record_flip_wait(wait, latency);
                    targets.targets.retire_flip();
                }
                Err(wait) => {
                    renderer.record_flip_wait(wait, None);
                    return Err(format!(
                        "the flip did not retire within {} ms",
                        wait.as_millis()
                    ));
                }
            }
        }
        let Some((slot, target)) = targets.targets.render_slot() else {
            return Err("no free scanout buffer".to_owned());
        };
        let mut target = target.clone();
        let full = [Region::new(0, 0, size.0, size.1)];
        // The screen canvas changed only where it has pending damage: while
        // layers were shown it was not drawn into, and its texture still
        // holds it as it was.
        let written: Vec<Region> = match renderer.pending() {
            Some(rects) => rects
                .iter()
                .map(|rect| Region::new(rect.x, rect.y, rect.width, rect.height))
                .collect(),
            None => full.to_vec(),
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
        if let Some(layers) = self.layers.as_mut() {
            layers.uploaded();
        }
        let targets = self
            .targets
            .as_mut()
            .ok_or_else(|| "no scanout targets".to_owned())?;
        match entry
            .scanout
            .submit_direct_target(&mut targets.targets, slot)
        {
            DirectSubmit::Queued | DirectSubmit::Presented => {}
            DirectSubmit::Busy => return Ok(()),
            DirectSubmit::Failed(error) | DirectSubmit::HdrFailed(_, error) => return Err(error),
        }
        self.cursor = cursor_key;
        renderer.record_present(started.elapsed(), stats.upload_px, stats.draws);
        if shown == Shown::Screen {
            let revision = renderer.presented_revision().wrapping_add(1);
            renderer.finish_present(revision, true);
        }
        self.shown = Some(shown);
        Ok(())
    }
}
