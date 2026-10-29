//! A bare TTY: KMS outputs, libinput devices, a libseat session.
//!
//! The session (logind or seatd) hands out the device descriptors and pauses
//! everything on a VT switch. Each connected connector of the primary GPU
//! becomes an output driven by smithay's `DrmCompositor`: planes, direct
//! scanout of client buffers and the cursor plane where the hardware has
//! them, GLES composition of the rest, one frame per vertical blank.
//! libinput's events become [`InputEvent`]s. Decisions that need no hardware
//! live in [`devices`] and [`seat_diag`], where they are tested.

pub mod access_keys;
pub mod devices;
pub mod input_config;
mod input_source;
mod libinput;
pub mod seat_diag;
pub mod tablet;
mod topology;
mod touch_map;
mod wake_check;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, VrrSupport,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::LibinputSessionInterface;
use smithay::backend::renderer::ImportDma as _;
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session as _};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::Device as _;
use smithay::reexports::drm::control::{
    Device as _, Mode as DrmMode, ModeFlags, ModeTypeFlags, connector, crtc,
};
use smithay::reexports::input::Libinput as LibinputContext;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Point, Transform};

use super::{Backend, BackendError, failed};
use crate::output::{ModeRequest, OutputChange, apply_placement, find_mode, refresh_interval};
use crate::present::{Presentation, Shown};
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};
use devices::{ModeInfo, select_mode};
use wake_check::{LitState, WakeCheck, WakeStage, WakeVerdict};

/// Formats tried for an output's primary plane, best first.
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

pub use crate::config::DisplayConfig;

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
/// What a queued frame carries to its vertical blank: its clients'
/// presentation feedback.
type Queued = Option<Presentation>;
type Manager = DrmOutputManager<Allocator, Exporter, Queued, DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, Queued, DrmDeviceFd>;

/// One lit output.
struct Lit {
    output: Output,
    connector: connector::Handle,
    /// The modes the connector advertised when it was lit.
    modes: Vec<DrmMode>,
    surface: Surface,
    /// A frame is queued and its vertical blank has not come yet.
    queued: bool,
    /// The panel is switched off (its CRTC inactive); nothing is drawn.
    dark: bool,
    /// A panel built into the machine (eDP, LVDS, DSI): its wake is lit
    /// only once its backlight is unblanked too.
    builtin: bool,
    /// The wake this output is in, until it is judged and its first frame
    /// shown.
    wake: Option<Wake>,
    /// When the last frame was queued.
    queued_at: Instant,
    vrr: Vrr,
    /// When the next frame may be drawn though nothing was queued.
    next_frame: Instant,
    interval: Duration,
}

/// Variable refresh on one output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vrr {
    /// The connector cannot switch without a modeset, or the driver refused.
    Unable,
    Off,
    On,
}

/// One output's wake: its check, and whether it was judged lit and only
/// the first frame is awaited.
struct Wake {
    check: WakeCheck,
    judged: bool,
}

/// How long a queued frame may wait for its vertical blank in a wake
/// before the link to the display counts as down.
const LINK_WAIT: Duration = Duration::from_secs(2);

/// The primary GPU: its device, renderer and outputs.
struct Gpu {
    node: DrmNode,
    path: PathBuf,
    manager: Manager,
    renderer: GlesRenderer,
    outputs: HashMap<crtc::Handle, Lit>,
    token: RegistrationToken,
    /// The last topology line written, so it is written when it changes.
    topology: String,
}

struct Inner {
    session: LibSeatSession,
    libinput: LibinputContext,
    gpu: Option<Gpu>,
    config: DisplayConfig,
    active: bool,
}

/// The TTY backend. Created by [`DrmBackend::start`], which registers the
/// session, udev, libinput and the GPU in the compositor's loop.
pub struct DrmBackend {
    inner: Rc<RefCell<Inner>>,
    input: Rc<RefCell<libinput::Input>>,
}

impl DrmBackend {
    /// Take the seat, light the primary GPU's connected outputs and read
    /// the seat's input devices.
    pub fn start(
        handle: &LoopHandle<'static, LoopData>,
        state: &mut State,
        config: DisplayConfig,
    ) -> Result<Self, Error> {
        let (session, notifier) = LibSeatSession::new().map_err(|error| {
            let failure = seat_diag::diagnose_live(format!("{error:?}"));
            BackendError(failure.to_string())
        })?;
        let seat = session.seat();

        let mut libinput =
            LibinputContext::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput
            .udev_assign_seat(&seat)
            .map_err(|()| BackendError(format!("libinput: seat {seat} not assigned")))?;

        let inner = Rc::new(RefCell::new(Inner {
            session,
            libinput: libinput.clone(),
            gpu: None,
            config,
            active: true,
        }));

        let udev = UdevBackend::new(&seat).map_err(|error| failed("udev", error))?;
        let gpu_path = primary_gpu(&seat, &udev)?;
        let gpu = open_gpu(handle, &inner, &gpu_path)?;
        state.enable_dmabuf(
            gpu.renderer.dmabuf_formats().iter().copied().collect(),
            Some(gpu.node),
        );
        inner.borrow_mut().gpu = Some(gpu);
        scan_connectors(&inner, state);

        let input = Rc::new(RefCell::new(libinput::Input::new(
            input_config::InputConfig::default(),
        )));
        let devices = Rc::clone(&input);
        let input_handle = handle.clone();
        handle
            .insert_source(
                input_source::Source::new(libinput),
                move |delivered, (), data| match delivered {
                    input_source::Delivered::Device(event) => {
                        let extent = pointer_extent(&data.state);
                        libinput::dispatch(&devices, &input_handle, event, &mut data.state, extent);
                    }
                    input_source::Delivered::Pad { device, event } => {
                        libinput::dispatch_pad(&devices, &device, &event, &mut data.state);
                    }
                },
            )
            .map_err(|error| failed("libinput source", error.error))?;

        let session_inner = Rc::clone(&inner);
        handle
            .insert_source(notifier, move |event, (), data| match event {
                SessionEvent::PauseSession => pause(&session_inner),
                SessionEvent::ActivateSession => {
                    resume(&session_inner);
                    scan_connectors(&session_inner, &mut data.state);
                }
            })
            .map_err(|error| failed("session source", error.error))?;

        let udev_inner = Rc::clone(&inner);
        let udev_handle = handle.clone();
        handle
            .insert_source(udev, move |event, (), data| match event {
                UdevEvent::Changed { device_id } => {
                    let ours = udev_inner
                        .borrow()
                        .gpu
                        .as_ref()
                        .is_some_and(|gpu| gpu.node.dev_id() == device_id);
                    if ours {
                        scan_connectors(&udev_inner, &mut data.state);
                    }
                }
                UdevEvent::Added { path, .. } => {
                    tracing::info!(path = %path.display(), "GPU added; not driven");
                }
                UdevEvent::Removed { device_id } => {
                    let ours = udev_inner
                        .borrow()
                        .gpu
                        .as_ref()
                        .is_some_and(|gpu| gpu.node.dev_id() == device_id);
                    if ours {
                        tracing::warn!("primary GPU removed");
                        remove_gpu(&udev_inner, &udev_handle, &mut data.state);
                    }
                }
            })
            .map_err(|error| failed("udev source", error.error))?;

        Ok(Self { inner, input })
    }
}

impl Backend for DrmBackend {
    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        render_ready(&self.inner, state);
        Ok(())
    }

    fn configure(
        &mut self,
        state: &mut State,
        changes: &[OutputChange],
        test: bool,
    ) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        let gpu = inner
            .gpu
            .as_mut()
            .ok_or_else(|| BackendError("no GPU is driven".into()))?;
        // Every mode is resolved before anything changes: all or nothing.
        let mut plan = Vec::with_capacity(changes.len());
        for change in changes {
            let (crtc, lit) = gpu
                .outputs
                .iter()
                .find(|(_, lit)| lit.output.name() == change.name)
                .ok_or_else(|| BackendError(format!("{}: not lit", change.name)))?;
            let mode = match change.mode {
                Some(request) => Some(drm_mode(&lit.modes, request).ok_or_else(|| {
                    BackendError(format!("{}: the display has no such mode", change.name))
                })?),
                None => None,
            };
            plan.push((*crtc, mode, change));
        }
        if test {
            return Ok(());
        }
        for (crtc, mode, change) in plan {
            let Some(lit) = gpu.outputs.get_mut(&crtc) else {
                continue;
            };
            if let Some(mode) = mode {
                lit.surface
                    .use_mode::<_, OutputRenderElement<GlesRenderer>>(
                        mode,
                        &mut gpu.renderer,
                        &DrmOutputRenderElements::default(),
                    )
                    .map_err(|error| failed(&format!("{}: mode", change.name), error))?;
                let output_mode = OutputMode::from(mode);
                lit.output
                    .change_current_state(Some(output_mode), None, None, None);
                lit.interval = refresh_interval(output_mode.refresh);
            }
            apply_placement(state, &lit.output, change);
            lit.next_frame = Instant::now();
            if !change.enabled
                && let Some(mode) = lit.output.current_mode()
            {
                let size = (
                    u16::try_from(mode.size.w).unwrap_or(0),
                    u16::try_from(mode.size.h).unwrap_or(0),
                );
                tracing::info!(
                    "{}",
                    topology::connector_attempt_line(
                        &gpu.path,
                        &change.name,
                        size,
                        topology::ConnectorAttempt::Disabled
                    )
                );
            }
        }
        log_topology(gpu, state);
        Ok(())
    }

    fn set_power(&mut self, state: &mut State, name: &str, on: bool) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        let gpu = inner
            .gpu
            .as_mut()
            .ok_or_else(|| BackendError("no GPU is driven".into()))?;
        // A configuration that turned an output off or on changes what is
        // driven.
        log_topology(gpu, state);
        let lit = gpu
            .outputs
            .values_mut()
            .find(|lit| lit.output.name() == name)
            .ok_or_else(|| BackendError(format!("{name}: not lit")))?;
        if on != lit.dark {
            return Ok(());
        }
        if on {
            // The next queued frame commits the CRTC active again; it has
            // to be a whole frame, the old buffers are gone.
            lit.surface.reset_buffers();
            lit.next_frame = Instant::now();
            lit.begin_wake("dpms");
        } else {
            lit.surface
                .with_compositor(smithay::backend::drm::compositor::DrmCompositor::clear)
                .map_err(|error| failed(&format!("{name}: switch off"), error))?;
            lit.queued = false;
        }
        lit.dark = !on;
        tracing::info!(output = name, on, "panel power");
        drop(inner);
        if on {
            // A wake is the person asking for the displays: a connected one
            // that could not be lit before gets another attempt.
            scan_connectors(&self.inner, state);
        }
        Ok(())
    }

    fn set_display_config(&mut self, config: &DisplayConfig) {
        self.inner.borrow_mut().config = config.clone();
    }

    fn set_input_config(&mut self, state: &mut State, config: input_config::InputConfig) {
        self.input.borrow_mut().set_config(config, state);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let inner = self.inner.borrow();
        let gpu = inner.gpu.as_ref()?;
        gpu.outputs
            .values()
            .filter(|lit| !lit.queued && !lit.dark)
            .map(|lit| lit.next_frame)
            .min()
    }
}

/// Draw every output that is not waiting for a vertical blank and whose
/// next frame is due.
fn render_ready(inner: &Rc<RefCell<Inner>>, state: &mut State) {
    let mut inner = inner.borrow_mut();
    if !inner.active {
        return;
    }
    let Inner { gpu, config, .. } = &mut *inner;
    let variable_refresh = &config.variable_refresh;
    let Some(gpu) = gpu.as_mut() else {
        return;
    };
    let now = Instant::now();
    for lit in gpu.outputs.values_mut() {
        if lit.queued
            && lit.wake.as_ref().is_some_and(|wake| !wake.judged)
            && now.saturating_duration_since(lit.queued_at) > LINK_WAIT
        {
            // No vertical blank after the modeset: the link did not come up.
            judge_wake(
                lit,
                gpu.manager.device(),
                &**state.idle.dimmer.backlight(),
                Some(WakeStage::Link),
            );
        }
        if lit.queued || lit.dark || now < lit.next_frame {
            continue;
        }
        let name = lit.output.name();
        lit.follow_vrr(vrr_wanted(
            lit.vrr != Vrr::Unable,
            variable_refresh.contains(&name),
            state.fullscreen_on(&lit.output),
        ));
        // The pointer in front, where DrmCompositor can give it the cursor
        // plane.
        let mut elements: Vec<OutputRenderElement<GlesRenderer>> =
            state.cursor_elements(&mut gpu.renderer, &lit.output);
        elements.extend(output_elements(&mut gpu.renderer, state, &lit.output));
        match lit.surface.render_frame(
            &mut gpu.renderer,
            &elements,
            BACKGROUND,
            FrameFlags::DEFAULT,
        ) {
            Ok(frame) => {
                // Clients draw their next frame now; an output with nothing
                // new is asked again one refresh later.
                let presentation = state.frame_drawn(&lit.output, Some(&frame.states));
                if frame.is_empty {
                    presentation.shown(Shown::Host);
                } else {
                    match lit.surface.queue_frame(Some(presentation)) {
                        Ok(()) => {
                            lit.queued = true;
                            lit.queued_at = now;
                        }
                        Err(error) => {
                            tracing::warn!(?error, output = lit.output.name(), "frame not queued");
                            if lit.wake.is_some() {
                                judge_wake(
                                    lit,
                                    gpu.manager.device(),
                                    &**state.idle.dimmer.backlight(),
                                    Some(WakeStage::Commit),
                                );
                            }
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(?error, output = lit.output.name(), "frame not rendered");
                state.frame_drawn(&lit.output, None).shown(Shown::Host);
            }
        }
        lit.next_frame = now.checked_add(lit.interval).unwrap_or(now);
    }
}

fn vblank(
    inner: &Rc<RefCell<Inner>>,
    state: &State,
    crtc: crtc::Handle,
    metadata: Option<&DrmEventMetadata>,
) {
    let mut inner = inner.borrow_mut();
    let Some(gpu) = inner.gpu.as_mut() else {
        return;
    };
    let device = gpu.manager.device();
    let Some(lit) = gpu.outputs.get_mut(&crtc) else {
        return;
    };
    match lit.surface.frame_submitted() {
        Ok(Some(Some(presentation))) => presentation.shown(vblank_shown(metadata)),
        Ok(_) => {}
        Err(error) => tracing::warn!(?error, output = lit.output.name(), "page flip not retired"),
    }
    lit.queued = false;
    lit.next_frame = Instant::now();
    match lit.wake.as_mut() {
        // The link is up; what the kernel holds says the rest.
        Some(wake) if !wake.judged => {
            judge_wake(lit, device, &**state.idle.dimmer.backlight(), None);
        }
        Some(wake) => {
            if let Some(line) = wake
                .check
                .first_flip(u32::from(lit.connector), Instant::now())
            {
                tracing::info!("{line}");
            }
            if wake.check.settled() {
                lit.wake = None;
            }
        }
        None => {}
    }
}

/// Variable refresh engages only for a fullscreen window on an output that
/// allows and supports it. The desktop stays at the fixed rate: uneven UI
/// frame times make many panels flicker in brightness when the refresh rate
/// follows them.
fn vrr_wanted(capable: bool, allowed: bool, fullscreen_here: bool) -> bool {
    capable && allowed && fullscreen_here
}

impl Lit {
    /// Engage or release variable refresh for the next frame. A driver that
    /// refuses it is not asked again for this output.
    fn follow_vrr(&mut self, wanted: bool) {
        if self.vrr == Vrr::Unable || wanted == (self.vrr == Vrr::On) {
            return;
        }
        match self
            .surface
            .with_compositor(|compositor| compositor.use_vrr(wanted))
        {
            Ok(()) => {
                self.vrr = if wanted { Vrr::On } else { Vrr::Off };
                tracing::info!(output = self.output.name(), on = wanted, "variable refresh");
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    output = self.output.name(),
                    "variable refresh refused"
                );
                self.vrr = Vrr::Unable;
            }
        }
    }

    /// The output is asked to light after being dark (`reason`: `dpms`,
    /// `resume`): its wake is judged once the light-up is known.
    fn begin_wake(&mut self, reason: &'static str) {
        if self.wake.is_none() {
            self.wake = Some(Wake {
                check: WakeCheck::new(Instant::now(), reason, None),
                judged: false,
            });
        }
    }
}

/// Judge `lit`'s wake, `observed` the stage seen not to happen (else read
/// back from the kernel), and write its record. A first failure redoes the
/// modeset from the CRTC switched off, which retrains the link and
/// re-enables the panel; a second is final for this wake.
fn judge_wake(
    lit: &mut Lit,
    device: &DrmDevice,
    backlight: &dyn crate::dim::Backlight,
    observed: Option<WakeStage>,
) {
    let Some(wake) = lit.wake.as_mut() else {
        return;
    };
    let read = LitState {
        crtc_active: crtc_active(device, lit.surface.crtc()),
        connector: match device
            .get_connector(lit.connector, false)
            .map(|info| info.state())
        {
            Ok(connector::State::Connected) => "connected",
            Ok(connector::State::Disconnected) => "disconnected",
            _ => "unknown",
        },
        backlight_powered: lit.builtin.then(|| backlight.powered()).flatten(),
        backlight: lit.builtin.then(|| backlight.describe()).flatten(),
    };
    let verdict = wake.check.judge(observed.or_else(|| read.failed_stage()));
    let line = wake
        .check
        .record(u32::from(lit.connector), verdict, &read, Instant::now());
    match verdict {
        WakeVerdict::Lit => {
            tracing::info!("{line}");
            wake.judged = true;
        }
        WakeVerdict::Redo(_) => {
            tracing::warn!("{line}");
            if let Err(error) = lit
                .surface
                .with_compositor(smithay::backend::drm::compositor::DrmCompositor::clear)
            {
                tracing::warn!(
                    ?error,
                    output = lit.output.name(),
                    "CRTC not switched off for the redo"
                );
            }
            lit.surface.reset_buffers();
            lit.queued = false;
            lit.next_frame = Instant::now();
        }
        WakeVerdict::Failed(_) => {
            tracing::warn!("{line}");
            lit.wake = None;
        }
    }
}

/// Whether `crtc` reads back active (its atomic `ACTIVE`, else a bound
/// mode); `None` when it cannot be read.
fn crtc_active(device: &DrmDevice, crtc: crtc::Handle) -> Option<bool> {
    let active = device.get_properties(crtc).ok().and_then(|properties| {
        properties.iter().find_map(|(handle, value)| {
            let info = device.get_property(*handle).ok()?;
            (info.name().to_bytes() == b"ACTIVE").then_some(*value != 0)
        })
    });
    active.or_else(|| device.get_crtc(crtc).ok().map(|info| info.mode().is_some()))
}

/// How a vertical blank reports its frame: the kernel's monotonic time and
/// counter, or our clock when it gave none (or a realtime stamp).
fn vblank_shown(metadata: Option<&DrmEventMetadata>) -> Shown {
    match metadata {
        Some(DrmEventMetadata {
            time: DrmEventTime::Monotonic(time),
            sequence,
        }) => Shown::Vblank {
            time: *time,
            sequence: *sequence,
        },
        _ => Shown::Host,
    }
}

fn pause(inner: &Rc<RefCell<Inner>>) {
    let mut inner = inner.borrow_mut();
    inner.active = false;
    inner.libinput.suspend();
    if let Some(gpu) = inner.gpu.as_mut() {
        gpu.manager.pause();
    }
}

fn resume(inner: &Rc<RefCell<Inner>>) {
    let mut inner = inner.borrow_mut();
    if inner.libinput.resume().is_err() {
        tracing::warn!("libinput did not resume");
    }
    if let Some(gpu) = inner.gpu.as_mut() {
        if let Err(error) = gpu.manager.activate(false) {
            tracing::warn!(?error, "GPU did not reactivate");
        }
        for lit in gpu.outputs.values_mut() {
            // What was on screen before the switch is gone.
            lit.surface.reset_buffers();
            lit.queued = false;
            lit.next_frame = Instant::now();
            if !lit.dark {
                lit.begin_wake("resume");
            }
        }
    }
    inner.active = true;
}

fn remove_gpu(
    inner: &Rc<RefCell<Inner>>,
    handle: &LoopHandle<'static, LoopData>,
    state: &mut State,
) {
    let gpu = inner.borrow_mut().gpu.take();
    if let Some(gpu) = gpu {
        for lit in gpu.outputs.values() {
            state.remove_output(&lit.output);
        }
        handle.remove(gpu.token);
    }
}

/// The GPU whose connectors to drive: udev's primary, else the one with the
/// most connected connectors.
fn primary_gpu(seat: &str, udev: &UdevBackend) -> Result<PathBuf, BackendError> {
    if let Ok(Some(path)) = udev::primary_gpu(seat) {
        return Ok(path);
    }
    let candidates =
        devices::filter_kms_nodes(udev.device_list().map(|(_, path)| path.to_path_buf()));
    devices::order_by_connected_connectors(candidates, Path::new("/sys/class/drm"))
        .into_iter()
        .next()
        .ok_or_else(|| BackendError(format!("no KMS device on seat {seat}")))
}

fn open_gpu(
    handle: &LoopHandle<'static, LoopData>,
    inner: &Rc<RefCell<Inner>>,
    path: &Path,
) -> Result<Gpu, BackendError> {
    let fd = inner
        .borrow_mut()
        .session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|error| failed(&format!("open {}", path.display()), error))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(fd.clone()).map_err(|error| failed("DRM node", error))?;
    let (device, notifier) =
        DrmDevice::new(fd.clone(), true).map_err(|error| failed("DRM device", error))?;
    let gbm = GbmDevice::new(fd).map_err(|error| failed("GBM device", error))?;
    // SAFETY: the display is created from this GPU's GBM device, which
    // outlives it inside the renderer.
    let display =
        unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|error| failed("EGL display", error))?;
    let context = EGLContext::new(&display).map_err(|error| failed("EGL context", error))?;
    let renderer = gles_renderer(context, &device)?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
    let renderer_formats = renderer
        .dmabuf_formats()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    let manager = Manager::new(
        device,
        allocator,
        exporter,
        Some(gbm),
        COLOR_FORMATS,
        renderer_formats,
    );

    let vblank_inner = Rc::clone(inner);
    let token = handle
        .insert_source(notifier, move |event, metadata, data| match event {
            DrmEvent::VBlank(crtc) => {
                vblank(&vblank_inner, &data.state, crtc, metadata.as_ref());
                render_ready(&vblank_inner, &mut data.state);
            }
            DrmEvent::Error(error) => tracing::warn!(?error, "DRM device error"),
        })
        .map_err(|error| failed("DRM source", error.error))?;
    Ok(Gpu {
        node,
        path: path.to_path_buf(),
        topology: String::new(),
        manager,
        renderer,
        outputs: HashMap::new(),
        token,
    })
}

/// The GPU's renderer. On panfrost (Mali) it runs without per-texture
/// fences: such a fence makes the driver submit every draw as its own
/// batch, each reloading and writing back the whole framebuffer; KMS waits
/// for the frame through the scanout buffer's implicit fence instead.
fn gles_renderer(context: EGLContext, device: &DrmDevice) -> Result<GlesRenderer, BackendError> {
    let panfrost = device
        .get_driver()
        .is_ok_and(|driver| driver.name() == "panfrost");
    // SAFETY: the context is current only on this thread, the loop's.
    let renderer = if panfrost {
        let capabilities = unsafe { GlesRenderer::supported_capabilities(&context) }
            .map_err(|error| failed("GLES capabilities", error))?
            .into_iter()
            .filter(|capability| {
                !matches!(capability, Capability::Fencing | Capability::ExportFence)
            });
        unsafe { GlesRenderer::with_capabilities(context, capabilities) }
    } else {
        unsafe { GlesRenderer::new(context) }
    };
    renderer.map_err(|error| failed("GLES renderer", error))
}

/// Light connectors that became connected, drop those that went away.
fn scan_connectors(inner: &Rc<RefCell<Inner>>, state: &mut State) {
    let mut inner = inner.borrow_mut();
    let config = inner.config.clone();
    let Some(gpu) = inner.gpu.as_mut() else {
        return;
    };
    let device = gpu.manager.device();
    let Ok(resources) = device.resource_handles() else {
        tracing::warn!("DRM resources unreadable");
        return;
    };
    let connected: Vec<connector::Info> = resources
        .connectors()
        .iter()
        .filter_map(|handle| device.get_connector(*handle, false).ok())
        .filter(|info| info.state() == connector::State::Connected)
        .collect();
    let connected_handles: HashSet<connector::Handle> =
        connected.iter().map(connector::Info::handle).collect();

    let gone: Vec<crtc::Handle> = gpu
        .outputs
        .iter()
        .filter(|(_, lit)| !connected_handles.contains(&lit.connector))
        .map(|(crtc, _)| *crtc)
        .collect();
    for crtc in gone {
        if let Some(lit) = gpu.outputs.remove(&crtc) {
            tracing::info!(output = lit.output.name(), "output disconnected");
            state.remove_output(&lit.output);
        }
    }

    let mut used: HashSet<crtc::Handle> = gpu.outputs.keys().copied().collect();
    let lit_connectors: HashSet<connector::Handle> =
        gpu.outputs.values().map(|lit| lit.connector).collect();
    for info in connected
        .iter()
        .filter(|info| !lit_connectors.contains(&info.handle()))
    {
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let modes: Vec<ModeInfo> = info
            .modes()
            .iter()
            .map(|mode| ModeInfo {
                width: mode.size().0,
                height: mode.size().1,
                refresh_mhz: mode.vrefresh().saturating_mul(1000),
                preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
                interlaced: mode.flags().contains(ModeFlags::INTERLACE),
            })
            .collect();
        let Some(mode) = select_mode(&modes, config.modes.get(&name).copied())
            .and_then(|index| info.modes().get(index))
        else {
            tracing::warn!(output = name, "connector advertises no usable mode");
            continue;
        };
        let attempt = |result| {
            tracing::info!(
                "{}",
                topology::connector_attempt_line(&gpu.path, &name, mode.size(), result)
            );
        };
        let Some(crtc) = free_crtc(gpu.manager.device(), &resources, info, &used) else {
            attempt(topology::ConnectorAttempt::Failed(
                "no free CRTC can drive this connector",
            ));
            continue;
        };
        let scale = config.scales.get(&name).copied().unwrap_or(1.0);
        let lit = light(gpu, state, info, &name, crtc, *mode, scale);
        let line = topology::connector_attempt_line(
            &gpu.path,
            &name,
            mode.size(),
            if lit.is_some() {
                topology::ConnectorAttempt::Lit
            } else {
                topology::ConnectorAttempt::Failed("the modeset was refused")
            },
        );
        tracing::info!("{line}");
        if let Some(lit) = lit {
            used.insert(crtc);
            if let Some(transform) = config.transforms.get(&name) {
                lit.output
                    .change_current_state(None, Some(*transform), None, None);
            }
            if config.disabled.contains(&name) {
                // Turned off by the person: known, not shown; its panel
                // goes dark at the next power sync.
                state.disable_output(&name);
            }
            gpu.outputs.insert(crtc, lit);
        }
    }
    arrange_outputs(gpu, &config, state);
    log_topology(gpu, state);
}

/// Write the topology line when what is driven changed.
fn log_topology(gpu: &mut Gpu, state: &State) {
    let cards = std::collections::BTreeMap::from([(gpu.node.dev_id(), gpu.path.clone())]);
    let mut driven: Vec<topology::Driven> = gpu
        .outputs
        .values()
        .filter(|lit| !state.disabled_outputs.contains(&lit.output))
        .map(|lit| {
            let mode = lit.output.current_mode();
            let position = lit.output.current_location();
            topology::Driven {
                connector_name: lit.output.name(),
                device: gpu.node.dev_id(),
                size: mode.map_or((0, 0), |mode| (mode.size.w, mode.size.h)),
                refresh_mhz: mode.map_or(0, |mode| mode.refresh),
                logical_position: (position.x, position.y),
                scale: lit.output.current_scale().fractional_scale(),
            }
        })
        .collect();
    driven.sort_by_key(|output| (output.logical_position, output.connector_name.clone()));
    let line = topology::topology_line(&cards, &driven);
    if line != gpu.topology {
        tracing::info!("{line}");
        gpu.topology = line;
    }
}

/// Place every lit output: where the configuration says, the rest packed
/// left to right in their current order after the configured ones, so an
/// unplugged display leaves no hole for the pointer to get lost in.
fn arrange_outputs(gpu: &Gpu, config: &DisplayConfig, state: &mut State) {
    // Outputs a configuration turned off keep their place for when they
    // come back but take none in the layout.
    let mut lit: Vec<&Output> = gpu
        .outputs
        .values()
        .map(|lit| &lit.output)
        .filter(|output| !state.disabled_outputs.contains(output))
        .collect();
    lit.sort_by_key(|output| (output.current_location().x, output.name()));
    let outputs: Vec<(String, i32)> = lit
        .iter()
        .map(|output| (output.name(), logical_width(output)))
        .collect();
    let positions = crate::output::arrange(&outputs, &config.positions);
    for (output, position) in lit.into_iter().zip(positions) {
        if output.current_location() != Point::from(position) {
            output.change_current_state(None, None, None, Some(position.into()));
            state.space.map_output(output, position);
        }
    }
    state.rearrange();
}

/// An output's width in the logical layout: its mode over its scale,
/// turned.
fn logical_width(output: &Output) -> i32 {
    let size = output
        .current_mode()
        .map(|mode| output.current_transform().transform_size(mode.size))
        .unwrap_or_default();
    size.to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_round::<i32>()
        .w
}

/// Drive `info`'s connector from `crtc` in `mode` and make it an output
/// right of the others.
fn light(
    gpu: &mut Gpu,
    state: &mut State,
    info: &connector::Info,
    name: &str,
    crtc: crtc::Handle,
    mode: DrmMode,
    scale: f64,
) -> Option<Lit> {
    let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
    let output = Output::new(
        name.to_owned(),
        PhysicalProperties {
            size: (
                i32::try_from(mm_w).unwrap_or(0),
                i32::try_from(mm_h).unwrap_or(0),
            )
                .into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".into(),
            model: name.to_owned(),
        },
    );
    let output_mode = OutputMode::from(mode);
    // Right of the others until the arrangement places it.
    let location = Point::from((next_x(state), 0));
    output.change_current_state(
        Some(output_mode),
        Some(Transform::Normal),
        Some(Scale::Fractional(scale)),
        Some(location),
    );
    for advertised in info.modes() {
        output.add_mode(OutputMode::from(*advertised));
    }
    output.set_preferred(output_mode);
    let surface = gpu
        .manager
        .initialize_output::<_, OutputRenderElement<GlesRenderer>>(
            crtc,
            mode,
            &[info.handle()],
            &output,
            None,
            &mut gpu.renderer,
            &DrmOutputRenderElements::default(),
        )
        .map_err(|error| tracing::warn!(?error, output = name, "output not lit"))
        .ok()?;
    let vrr = if surface
        .with_compositor(|compositor| compositor.vrr_supported(info.handle()))
        .is_ok_and(|support| support == VrrSupport::Supported)
    {
        Vrr::Off
    } else {
        Vrr::Unable
    };
    crate::output::advertise(&output, &state.display);
    state.add_output(&output, location);
    let interval = refresh_interval(output_mode.refresh);
    tracing::info!(output = name, ?interval, "output lit");
    Some(Lit {
        output,
        connector: info.handle(),
        modes: info.modes().to_vec(),
        surface,
        queued: false,
        dark: false,
        builtin: matches!(
            info.interface(),
            connector::Interface::EmbeddedDisplayPort
                | connector::Interface::LVDS
                | connector::Interface::DSI
        ),
        wake: None,
        queued_at: Instant::now(),
        vrr,
        next_frame: Instant::now(),
        interval,
    })
}

/// The advertised mode a request names (see [`crate::output::find_mode`]).
fn drm_mode(modes: &[DrmMode], request: ModeRequest) -> Option<DrmMode> {
    let as_output: Vec<OutputMode> = modes.iter().copied().map(OutputMode::from).collect();
    let wanted = find_mode(&as_output, request)?;
    modes
        .iter()
        .copied()
        .find(|mode| OutputMode::from(*mode) == wanted)
}

fn free_crtc(
    device: &DrmDevice,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    info: &connector::Info,
    used: &HashSet<crtc::Handle>,
) -> Option<crtc::Handle> {
    info.encoders()
        .iter()
        .filter_map(|encoder| device.get_encoder(*encoder).ok())
        .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
        .find(|crtc| !used.contains(crtc))
}

/// Where the next output goes: right of every output already laid out.
fn next_x(state: &State) -> i32 {
    state
        .output_geometries()
        .iter()
        .map(|geometry| geometry.loc.x.saturating_add(geometry.size.w))
        .max()
        .unwrap_or(0)
}

/// The logical size absolute pointer devices map onto: the first output.
fn pointer_extent(state: &State) -> smithay::utils::Size<i32, smithay::utils::Logical> {
    state
        .output_geometries()
        .first()
        .map_or_else(|| (1, 1).into(), |geometry| geometry.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_refresh_follows_a_fullscreen_window_where_allowed_and_capable() {
        assert!(vrr_wanted(true, true, true));
        assert!(
            !vrr_wanted(true, true, false),
            "the desktop keeps a fixed rate"
        );
        assert!(!vrr_wanted(true, false, true), "the person turned it off");
        assert!(!vrr_wanted(false, true, true), "the display cannot");
    }
}
