//! A toolkit host for a screen with no compositor: a `ui` [`App`] drawn by
//! the CPU rasterizer into KMS dumb buffers and driven by libinput. No winit
//! event loop, no wgpu, no Wayland. The sign-in screen in the initramfs runs
//! on it (docs/security/sign-in.md §3.4), as does the greeter when no GPU
//! starts.
//!
//! The host takes descriptors already open, the DRM card (its display's
//! master) and the input devices, so the process running it needs no
//! privilege. Frames are drawn when the app asks for one, only where they
//! changed, and shown at the display's vertical blank.

mod canvas;
pub mod display;
mod input;
mod keyboard;

use std::collections::HashMap;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use ui::time::Instant;

use rustix::event::{EventfdFlags, PollFd, PollFlags};
use ui::app::{AppSurface, SurfaceDamage};
use ui::{App, InputDevice as Device, InteractionEnvironment};
use winit::event::TouchPhase;

pub use display::{Display, DisplayError};
pub use keyboard::Layout;

/// The descriptors the host runs on.
pub struct Devices {
    /// The DRM card, its display's master.
    pub card: OwnedFd,
    /// The input devices, each with the `/dev/input` path libinput knows it
    /// by.
    pub inputs: Vec<(PathBuf, OwnedFd)>,
}

/// What the host was asked from another thread.
#[derive(Default)]
struct Asked {
    external: Vec<String>,
    inputs: Vec<(PathBuf, OwnedFd)>,
    quit: bool,
}

struct Shared {
    wake: OwnedFd,
    asked: Mutex<Asked>,
}

impl Shared {
    fn ask(&self, ask: impl FnOnce(&mut Asked)) {
        ask(&mut self.asked.lock().unwrap_or_else(PoisonError::into_inner));
        self.wake();
    }

    fn wake(&self) {
        let _ = rustix::io::write(&self.wake, &1_u64.to_ne_bytes());
    }
}

/// Reaches a running host from any thread.
#[derive(Clone)]
pub struct Proxy(Arc<Shared>);

impl Proxy {
    /// Run the app's next pass: its model woke.
    pub fn wake(&self) {
        self.0.wake();
    }

    /// Hand `payload` to the app as an external event
    /// ([`ui::widget::Widget::on_external_event`]).
    pub fn send_external(&self, payload: String) {
        self.0.ask(|asked| asked.external.push(payload));
    }

    /// Read a device that appeared after the host started.
    pub fn add_input(&self, path: PathBuf, fd: OwnedFd) {
        self.0.ask(|asked| asked.inputs.push((path, fd)));
    }

    /// Stop the host: [`Host::run`] returns.
    pub fn quit(&self) {
        self.0.ask(|asked| asked.quit = true);
    }
}

/// The surface the app is shown on: the whole display.
struct Surface {
    size: winit::dpi::PhysicalSize<u32>,
    scale: f64,
    redraw: std::cell::Cell<bool>,
}

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
    fn hosts_tab_chords(&self) -> bool {
        // There are no tabs here; the chords go to the app.
        true
    }
}

/// Why the host stopped other than by being asked to.
#[derive(Debug)]
pub enum HostError {
    Display(DisplayError),
    Io(std::io::Error),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Display(error) => write!(f, "display: {error}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<DisplayError> for HostError {
    fn from(error: DisplayError) -> Self {
        Self::Display(error)
    }
}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct Host {
    display: Display,
    input: input::Input,
    canvas: canvas::Canvas,
    shared: Arc<Shared>,
    surface: Surface,
    /// Where each finger on the screen last was, for its lift.
    touches: HashMap<u64, (f64, f64)>,
    /// Taps, drags and pinches as pointer events, as the platform window
    /// makes them.
    emulator: ui::TouchPointerEmulator,
}

/// Cores the rasterizer uses for a large region.
const RASTER_THREADS: u16 = 4;

impl Host {
    /// Light the display on `devices.card` and read `devices.inputs` with the
    /// keyboard `layout`. Nothing is shown before the first frame.
    ///
    /// # Errors
    ///
    /// When the card has no display to light or the kernel refuses a
    /// request; the caller then asks another way.
    pub fn new(devices: Devices, layout: &Layout) -> Result<Self, HostError> {
        let display = Display::open(devices.card)?;
        let size = display.size();
        let scale = settings_store::display_scale::derive_scale(size, display.physical_size());
        tracing::info!(width = size.0, height = size.1, scale, "display lit");
        let wake = rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .map_err(std::io::Error::from)?;
        Ok(Self {
            input: input::Input::new(devices.inputs, layout, size),
            canvas: canvas::Canvas::new(size, RASTER_THREADS),
            display,
            shared: Arc::new(Shared {
                wake,
                asked: Mutex::new(Asked::default()),
            }),
            surface: Surface {
                size: winit::dpi::PhysicalSize::new(size.0, size.1),
                scale,
                redraw: std::cell::Cell::new(true),
            },
            touches: HashMap::new(),
            emulator: ui::TouchPointerEmulator::default(),
        })
    }

    pub fn proxy(&self) -> Proxy {
        Proxy(Arc::clone(&self.shared))
    }

    /// The display's size in pixels and the scale the app is drawn at.
    pub fn geometry(&self) -> ((u32, u32), f64) {
        (self.display.size(), self.surface.scale)
    }

    /// The input devices attached, as the toolkit's interaction model has
    /// them: exactly what is there, so a screen with no keyboard knows.
    fn environment(&self, current: InteractionEnvironment) -> InteractionEnvironment {
        let present = self.input.present();
        let mut available = ui::InputAvailability::NONE;
        if present.keyboard {
            available = available.with(Device::Keyboard);
        }
        if present.pointer {
            available = available.with(Device::Pointer);
        }
        if present.touch {
            available = available.with(Device::Touch);
        }
        let mut environment = if present.touch && !present.pointer && !present.keyboard {
            InteractionEnvironment::TOUCH
        } else {
            current
        };
        environment.available = available;
        environment
    }

    /// Run `app` on the display until the [`Proxy`] says to stop.
    /// `presented` is told when the first frame is on screen.
    ///
    /// # Errors
    ///
    /// When the display or the wake cannot be read any more.
    pub fn run(mut self, app: &mut App, presented: impl FnOnce()) -> Result<(), HostError> {
        let mut presented = Some(presented);
        let mut damage = SurfaceDamage::default();
        let mut scene = vello::Scene::new();
        let mut wants = true;
        let mut next_frame = ui::time::now();
        app.set_interaction_environment(self.environment(app.interaction_environment()));
        loop {
            let now = ui::time::now();
            let asked = std::mem::take(
                &mut *self
                    .shared
                    .asked
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            if asked.quit {
                return Ok(());
            }
            let mut devices = false;
            for (path, fd) in asked.inputs {
                self.input.add(path, fd);
                devices = true;
            }
            for payload in asked.external {
                app.external_event(&payload);
                wants = true;
            }
            let mut translated = self.input.dispatch(now);
            translated.extend(self.input.repeats(now));
            for event in translated {
                match event {
                    input::Translated::Event(event) => {
                        app.surface_input(&self.surface, event, false);
                        wants = true;
                    }
                    input::Translated::Pointer(at) => self.display.set_cursor(Some(at)),
                    input::Translated::Touch {
                        id,
                        phase,
                        location,
                    } => {
                        // A finger hides the pointer, as on the desktop.
                        self.display.set_cursor(None);
                        let at = match (phase, location) {
                            (_, Some(at)) => {
                                self.touches.insert(id, at);
                                at
                            }
                            (TouchPhase::Ended | TouchPhase::Cancelled, None) => {
                                match self.touches.remove(&id) {
                                    Some(at) => at,
                                    None => continue,
                                }
                            }
                            _ => continue,
                        };
                        app.surface_touch(
                            &self.surface,
                            input::touch(id, phase, at),
                            &mut self.emulator,
                        );
                        wants = true;
                    }
                    input::Translated::Devices => devices = true,
                }
            }
            if app.surface_touch_hold(&self.surface, &mut self.emulator, now) {
                wants = true;
            }
            // Both volume keys have been held long enough, or a finger
            // rested while the screen reader is on.
            if app.tick_screen_reader(now) {
                wants = true;
            }
            if devices {
                app.set_interaction_environment(self.environment(app.interaction_environment()));
                wants = true;
            }
            wants |= self.surface.redraw.replace(false) || app.wake_pending() || due(app, now);
            if wants && !self.display.flip_pending() && now >= next_frame {
                app.surface_accessibility();
                scene.reset();
                wants = app.surface_frame_with_damage(&self.surface, &mut scene, &mut damage);
                let changed = damage.take();
                let regions = self.canvas.draw(&scene, changed.as_deref());
                if !regions.is_empty() || presented.is_some() {
                    self.display.present(self.canvas.pixels(), Some(&regions))?;
                    // An unpaced driver flips at once: no frame comes before
                    // the next refresh would have.
                    next_frame = now
                        .checked_add(self.display.refresh_interval())
                        .unwrap_or(now);
                }
                if let Some(presented) = presented.take() {
                    presented();
                }
            }
            self.wait(app, wants, next_frame)?;
        }
    }

    /// Sleep until a device, the display or the wake has something, or the
    /// next deadline.
    fn wait(&mut self, app: &App, wants: bool, next_frame: Instant) -> Result<(), HostError> {
        let now = ui::time::now();
        let deadline = if wants && !self.display.flip_pending() {
            Some(next_frame)
        } else {
            [
                self.input.deadline(),
                self.emulator.long_press_deadline(),
                next_deadline(app),
            ]
            .into_iter()
            .flatten()
            .min()
        };
        let timeout = deadline.map(|at| {
            rustix::time::Timespec::try_from(at.saturating_duration_since(now)).unwrap_or(
                rustix::time::Timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                },
            )
        });
        let (input, display) = (self.input.fd(), self.display.fd());
        let mut fds = [
            PollFd::new(&self.shared.wake, PollFlags::IN),
            PollFd::new(&input, PollFlags::IN),
            PollFd::new(&display, PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(HostError::Io(error.into())),
        }
        let [wake, _, display] = fds.map(|fd| fd.revents());
        if wake.contains(PollFlags::IN) {
            let mut count = [0_u8; 8];
            let _ = rustix::io::read(self.shared.wake.as_fd(), &mut count);
        }
        if display.contains(PollFlags::IN) {
            self.display.dispatch()?;
        }
        Ok(())
    }
}

/// When `app` next changes with nothing to bring it but time: a frame it
/// wants (something moving, a widget's deadline such as the clock's next
/// minute, a tooltip's dwell) or the screen reader's touch timing. The
/// host sleeps until then and no longer, so a screen at rest wakes only
/// for what it shows changing.
#[must_use]
pub fn next_deadline(app: &App) -> Option<Instant> {
    [app.surface_next_frame(), app.screen_reader_deadline()]
        .into_iter()
        .flatten()
        .min()
}

/// Whether a frame of `app` is due at `now` by time alone.
#[must_use]
pub fn due(app: &App, now: Instant) -> bool {
    app.surface_next_frame().is_some_and(|at| at <= now)
}
