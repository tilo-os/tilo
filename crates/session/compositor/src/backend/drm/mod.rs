//! A bare TTY: KMS outputs, libinput devices, a libseat session.
//!
//! The session (logind or seatd) hands out the device descriptors and pauses
//! everything on a VT switch. Each connected connector of the primary GPU
//! becomes an output driven by smithay's `DrmCompositor`: planes, direct
//! scanout of client buffers and the cursor plane where the hardware has
//! them, GLES composition of the rest, one frame per vertical blank.
//! libinput's events become [`InputEvent`]s. Decisions that need no hardware
//! live in [`devices`] and [`seat_diag`], where they are tested.

pub mod devices;
pub mod seat_diag;

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
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event as _, InputBackend,
    InputEvent as Libinput, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::ImportDma as _;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session as _};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{Device as _, ModeFlags, ModeTypeFlags, connector, crtc};
use smithay::reexports::input::Libinput as LibinputContext;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{DeviceFd, Point, Transform};

use super::{Backend, BackendError, failed};
use crate::input::InputEvent;
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};
use devices::{ModeInfo, RequestedMode, select_mode};

/// Formats tried for an output's primary plane, best first.
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

/// The refresh interval assumed when a mode reports none.
const FALLBACK_INTERVAL: Duration = Duration::from_micros(16_667);

/// What the person configured for the outputs, by connector name.
#[derive(Clone, Debug, Default)]
pub struct DisplayConfig {
    /// A chosen mode per connector (`eDP-1`).
    pub modes: HashMap<String, RequestedMode>,
    /// A chosen scale per connector; unlisted outputs run at 1.
    pub scales: HashMap<String, f64>,
}

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type Manager = DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, (), DrmDeviceFd>;

/// One lit output.
struct Lit {
    output: Output,
    global: GlobalId,
    connector: connector::Handle,
    surface: Surface,
    /// A frame is queued and its vertical blank has not come yet.
    queued: bool,
    /// When the next frame may be drawn though nothing was queued.
    next_frame: Instant,
    interval: Duration,
}

/// The primary GPU: its device, renderer and outputs.
struct Gpu {
    node: DrmNode,
    manager: Manager,
    renderer: GlesRenderer,
    outputs: HashMap<crtc::Handle, Lit>,
    token: RegistrationToken,
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
        state.enable_dmabuf(gpu.renderer.dmabuf_formats().iter().copied().collect());
        inner.borrow_mut().gpu = Some(gpu);
        scan_connectors(&inner, state);

        let input = LibinputInputBackend::new(libinput);
        handle
            .insert_source(input, |event, (), data| {
                if let Some(event) = translate(event, pointer_extent(&data.state)) {
                    data.state.handle_input(event);
                }
            })
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
                    tracing::info!(path = %path.display(), "GPU added; not driven")
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

        Ok(Self { inner })
    }
}

impl Backend for DrmBackend {
    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        render_ready(&self.inner, state);
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        let inner = self.inner.borrow();
        let gpu = inner.gpu.as_ref()?;
        gpu.outputs
            .values()
            .filter(|lit| !lit.queued)
            .map(|lit| lit.next_frame)
            .min()
    }
}

/// Draw every output that is not waiting for a vertical blank and whose
/// next frame is due.
fn render_ready(inner: &Rc<RefCell<Inner>>, state: &State) {
    let mut inner = inner.borrow_mut();
    if !inner.active {
        return;
    }
    let Some(gpu) = inner.gpu.as_mut() else {
        return;
    };
    let now = Instant::now();
    for lit in gpu.outputs.values_mut() {
        if lit.queued || now < lit.next_frame {
            continue;
        }
        let elements: Vec<OutputRenderElement<GlesRenderer>> =
            output_elements(&mut gpu.renderer, state, &lit.output);
        match lit.surface.render_frame(
            &mut gpu.renderer,
            &elements,
            BACKGROUND,
            FrameFlags::DEFAULT,
        ) {
            Ok(frame) if !frame.is_empty => match lit.surface.queue_frame(()) {
                Ok(()) => lit.queued = true,
                Err(error) => {
                    tracing::warn!(?error, output = lit.output.name(), "frame not queued")
                }
            },
            Ok(_) => {}
            Err(error) => tracing::warn!(?error, output = lit.output.name(), "frame not rendered"),
        }
        // Clients draw their next frame now; an output with nothing new is
        // asked again one refresh later.
        state.frame_shown(&lit.output);
        lit.next_frame = now.checked_add(lit.interval).unwrap_or(now);
    }
}

fn vblank(inner: &Rc<RefCell<Inner>>, crtc: crtc::Handle) {
    let mut inner = inner.borrow_mut();
    let Some(lit) = inner
        .gpu
        .as_mut()
        .and_then(|gpu| gpu.outputs.get_mut(&crtc))
    else {
        return;
    };
    if let Err(error) = lit.surface.frame_submitted() {
        tracing::warn!(?error, output = lit.output.name(), "page flip not retired");
    }
    lit.queued = false;
    lit.next_frame = Instant::now();
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
            state.remove_output(&lit.output, lit.global.clone());
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
    // SAFETY: the context is current only on this thread, the loop's.
    let renderer =
        unsafe { GlesRenderer::new(context) }.map_err(|error| failed("GLES renderer", error))?;
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
        .insert_source(notifier, move |event, _metadata, data| match event {
            DrmEvent::VBlank(crtc) => {
                vblank(&vblank_inner, crtc);
                render_ready(&vblank_inner, &data.state);
            }
            DrmEvent::Error(error) => tracing::warn!(?error, "DRM device error"),
        })
        .map_err(|error| failed("DRM source", error.error))?;
    Ok(Gpu {
        node,
        manager,
        renderer,
        outputs: HashMap::new(),
        token,
    })
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
            state.remove_output(&lit.output, lit.global.clone());
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
        let Some(crtc) = free_crtc(device, &resources, info, &used) else {
            tracing::warn!(output = name, "no free CRTC");
            continue;
        };
        let scale = config.scales.get(&name).copied().unwrap_or(1.0);
        let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (
                    i32::try_from(mm_w).unwrap_or(0),
                    i32::try_from(mm_h).unwrap_or(0),
                )
                    .into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: name.clone(),
            },
        );
        let output_mode = OutputMode::from(*mode);
        let location = Point::from((next_x(state), 0));
        output.change_current_state(
            Some(output_mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale)),
            Some(location),
        );
        output.set_preferred(output_mode);
        let surface = match gpu
            .manager
            .initialize_output::<_, OutputRenderElement<GlesRenderer>>(
                crtc,
                *mode,
                &[info.handle()],
                &output,
                None,
                &mut gpu.renderer,
                &DrmOutputRenderElements::default(),
            ) {
            Ok(surface) => surface,
            Err(error) => {
                tracing::warn!(?error, output = name, "output not lit");
                continue;
            }
        };
        let global = output.create_global::<State>(&state.display);
        state.add_output(&output, location);
        used.insert(crtc);
        let interval = refresh_interval(output_mode.refresh);
        tracing::info!(output = name, ?interval, "output lit");
        gpu.outputs.insert(
            crtc,
            Lit {
                output,
                global,
                connector: info.handle(),
                surface,
                queued: false,
                next_frame: Instant::now(),
                interval,
            },
        );
    }
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

fn refresh_interval(refresh_mhz: i32) -> Duration {
    u64::try_from(refresh_mhz)
        .ok()
        .filter(|mhz| *mhz > 0)
        .map_or(FALLBACK_INTERVAL, |mhz| {
            Duration::from_nanos(1_000_000_000_000 / mhz)
        })
}

/// The logical size absolute pointer devices map onto: the first output.
fn pointer_extent(state: &State) -> smithay::utils::Size<i32, smithay::utils::Logical> {
    state
        .output_geometries()
        .first()
        .map_or_else(|| (1, 1).into(), |geometry| geometry.size)
}

/// A libinput event as the compositor routes it.
fn translate<B: InputBackend>(
    event: Libinput<B>,
    extent: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> Option<InputEvent> {
    match event {
        Libinput::Keyboard { event } => Some(InputEvent::Key {
            keycode: event.key_code().raw().saturating_sub(8),
            pressed: event.state() == KeyState::Pressed,
        }),
        Libinput::PointerMotion { event } => {
            let delta = event.delta();
            Some(InputEvent::PointerMotionRelative {
                dx: delta.x,
                dy: delta.y,
            })
        }
        Libinput::PointerMotionAbsolute { event } => {
            let at = event.position_transformed(extent);
            Some(InputEvent::PointerMotion { x: at.x, y: at.y })
        }
        Libinput::PointerButton { event } => Some(InputEvent::PointerButton {
            button: event.button_code(),
            pressed: event.state() == ButtonState::Pressed,
        }),
        Libinput::PointerAxis { event } => {
            let amount = |axis| scroll_amount(&event, axis);
            Some(InputEvent::PointerAxis {
                horizontal: amount(Axis::Horizontal),
                vertical: amount(Axis::Vertical),
            })
        }
        _ => None,
    }
}

/// Logical pixels scrolled on `axis`: a wheel notch (120 in v120 units) is
/// 15 pixels, as clients expect; fingers and continuous devices report
/// pixels.
fn scroll_amount<B: InputBackend>(event: &B::PointerAxisEvent, axis: Axis) -> f64 {
    match event.source() {
        AxisSource::Wheel | AxisSource::WheelTilt => event.amount_v120(axis).map_or_else(
            || event.amount(axis).unwrap_or(0.0),
            |v120| v120 * 15.0 / 120.0,
        ),
        _ => event.amount(axis).unwrap_or(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_intervals_follow_the_mode_and_fall_back_to_60_hz() {
        assert_eq!(refresh_interval(60_000), Duration::from_nanos(16_666_666));
        assert_eq!(refresh_interval(120_000), Duration::from_nanos(8_333_333));
        assert_eq!(refresh_interval(0), FALLBACK_INTERVAL);
        assert_eq!(refresh_interval(-1), FALLBACK_INTERVAL);
    }
}
