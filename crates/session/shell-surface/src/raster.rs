//! Frames drawn on the CPU, off the loop's thread.
//!
//! The thread that runs the compositor's loop also answers everything the session is asked: a
//! program connecting, a window being created, an app being launched. Rasterizing a desktop on the
//! CPU takes that thread as long as the picture takes to draw, so a surface hands its scene to the
//! drawing threads here and goes on. The compositor shows the frame's previous pixels meanwhile,
//! and the frame wakes the loop when the new ones are in ([`SurfaceFrame::finished`]).
//!
//! A surface's draws are taken in order and one at a time: a scene handed over while its last one
//! is still drawing waits, and a newer one replaces it, so a surface that changes faster than it
//! draws draws its latest picture and never falls behind.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tilo_session_core::surface::{Region, SurfaceFrame};
use ui::offscreen::CpuSurface;
use vello::Scene;

/// Threads drawing surfaces' frames: each rasterizes one surface at a time with the cores its
/// [`CpuSurface`] uses, so two surfaces that changed together (the desktop and a bar) draw side by
/// side.
const DRAWING_THREADS: usize = 2;

/// One surface's frames, drawn on the drawing threads into `frame`.
pub(crate) struct OffThread {
    slot: Arc<Slot>,
    size: (u32, u32),
}

impl OffThread {
    /// Draw `canvas`'s frames into `frame` off this thread. `replaces` is the frame shown until
    /// the first of them is drawn (the surface's frame before it was resized): it stays drawing
    /// until then, so whoever waits for what the surface shows waits for the new frame.
    pub(crate) fn new(
        canvas: Box<CpuSurface>,
        frame: SurfaceFrame,
        replaces: Option<SurfaceFrame>,
    ) -> Self {
        let size = canvas.size();
        let replaces = replaces.map(|before| {
            let ticket = before.drawing();
            (before, ticket)
        });
        Self {
            slot: Arc::new(Slot {
                frame,
                canvas: Mutex::new(canvas),
                state: Mutex::new(State {
                    next: None,
                    queued: false,
                    replaces,
                }),
            }),
            size,
        }
    }

    /// The frame it draws into.
    pub(crate) fn frame(&self) -> &SurfaceFrame {
        &self.slot.frame
    }

    /// The size it draws in pixels.
    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Draw `scene` where `damage` says it changed since the last scene handed over (`None`: all of
    /// it), without waiting for the drawing.
    pub(crate) fn draw(&self, scene: &Scene, damage: Option<Vec<(u32, u32, u32, u32)>>) {
        let ticket = self.slot.frame.drawing();
        let mut state = self.slot.state();
        // A scene still waiting is replaced by this one; what changed is what either changed, since
        // the frame holds neither yet.
        let damage = match (state.next.take().map(|next| next.damage), damage) {
            (None, damage) => damage,
            (Some(Some(mut waiting)), Some(damage)) => {
                waiting.extend(damage);
                Some(waiting)
            }
            (Some(_), _) => None,
        };
        state.next = Some(Draw {
            scene: scene.clone(),
            damage,
            ticket,
        });
        if !state.queued {
            state.queued = true;
            drop(state);
            // While the shell prepares a frame, the draw starts once it is prepared: a later
            // picture of this surface in the same frame replaces this one before it is drawn.
            let slot = Arc::clone(&self.slot);
            tilo_session_core::surface::drawing::start(move || send(slot));
        }
    }
}

/// A surface's canvas and the draw it waits for, shared with the drawing threads.
struct Slot {
    frame: SurfaceFrame,
    /// What it draws with: only ever the thread drawing the slot holds it.
    canvas: Mutex<Box<CpuSurface>>,
    state: Mutex<State>,
}

impl Slot {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Draw what was handed over until nothing waits.
    fn run(&self) {
        loop {
            let next = {
                let mut state = self.state();
                let next = state.next.take();
                if next.is_none() {
                    state.queued = false;
                }
                drop(state);
                next
            };
            let Some(draw) = next else {
                return;
            };
            {
                let mut canvas = self.canvas.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(drawn) = canvas.draw(&draw.scene, draw.damage.as_deref()) {
                    let drawn: Vec<Region> = drawn
                        .into_iter()
                        .map(|(x, y, width, height)| Region::new(x, y, width, height))
                        .collect();
                    self.frame.set_pixels(canvas.pixels(), &drawn);
                }
            }
            let replaced = self.state().replaces.take();
            self.frame.finished(draw.ticket);
            if let Some((before, ticket)) = replaced {
                before.finished(ticket);
            }
        }
    }
}

/// What a slot holds between draws.
struct State {
    /// The latest scene handed over and not yet drawn.
    next: Option<Draw>,
    /// The slot is with the drawing threads: a scene handed over now is drawn after the one they
    /// draw.
    queued: bool,
    /// The frame shown until the first draw is in, and the draw it waits for there.
    replaces: Option<(SurfaceFrame, u64)>,
}

/// One scene to draw.
struct Draw {
    scene: Scene,
    damage: Option<Vec<(u32, u32, u32, u32)>>,
    ticket: u64,
}

/// Hand `slot` to the drawing threads, started with the first.
fn send(slot: Arc<Slot>) {
    static THREADS: OnceLock<Mutex<Sender<Arc<Slot>>>> = OnceLock::new();
    let sender = THREADS.get_or_init(|| {
        let (sender, receiver) = channel::<Arc<Slot>>();
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..DRAWING_THREADS {
            let receiver = Arc::clone(&receiver);
            #[allow(
                clippy::expect_used,
                reason = "a session that cannot start the threads its frames are drawn on cannot draw"
            )]
            std::thread::Builder::new()
                .name(format!("surface-raster-{index}"))
                .spawn(move || drawing_thread(&receiver))
                .expect("a thread to draw surfaces' frames on");
        }
        Mutex::new(sender)
    });
    #[allow(
        clippy::expect_used,
        reason = "the drawing threads never return while the channel is open: a panic aborts"
    )]
    sender
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .send(slot)
        .expect("the drawing threads run as long as the process");
}

fn drawing_thread(receiver: &Mutex<Receiver<Arc<Slot>>>) {
    loop {
        let next = receiver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv();
        let Ok(slot) = next else {
            return;
        };
        // A surface whose frame cannot be drawn would stand still with its last pixels while
        // everything else goes on: the session stops instead, as it did when the loop's own thread
        // drew it.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slot.run())).is_err() {
            std::process::abort();
        }
    }
}
