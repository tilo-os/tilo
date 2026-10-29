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
mod libinput;
pub mod seat_diag;
pub mod tablet;

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
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::ImportDma as _;
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session as _};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{
    Device as _, Mode as DrmMode, ModeFlags, ModeTypeFlags, connector, crtc,
};
use smithay::reexports::drm::Device as _;
use smithay::reexports::input::Libinput as LibinputContext;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{DeviceFd, Point, Transform};

use super::{Backend, BackendError, failed, refuse_disabling};
use crate::output::{
    ModeRequest, OutputChange, apply_placement, find_mode, refresh_interval,
};
use crate::present::{Presentation, Shown};
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};
use devices::{ModeInfo, RequestedMode, select_mode};

/// Formats tried for an output's primary plane, best first.
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

/// What the person configured for the outputs, by connector name.
#[derive(Clone, Debug, Default)]
pub struct DisplayConfig {
    /// A chosen mode per connector (`eDP-1`).
    pub modes: HashMap<String, RequestedMode>,
    /// A chosen scale per connector; unlisted outputs run at 1.
    pub scales: HashMap<String, f64>,
    /// A chosen top-left corner in the logical layout per connector;
    /// unlisted outputs are packed left to right after the listed ones.
    pub positions: HashMap<String, (i32, i32)>,
}

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
    global: GlobalId,
    connector: connector::Handle,
    /// The modes the connector advertised when it was lit.
    modes: Vec<DrmMode>,
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
                LibinputInputBackend::new(libinput),
                move |event, (), data| {
                    let extent = pointer_extent(&data.state);
                    libinput::dispatch(&devices, &input_handle, event, &mut data.state, extent);
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
        refuse_disabling(changes)?;
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
        }
        Ok(())
    }

    fn set_input_config(&mut self, state: &mut State, config: input_config::InputConfig) {
        self.input.borrow_mut().set_config(config, state);
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
fn render_ready(inner: &Rc<RefCell<Inner>>, state: &mut State) {
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
                        Ok(()) => lit.queued = true,
                        Err(error) => {
                            tracing::warn!(?error, output = lit.output.name(), "frame not queued");
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

fn vblank(inner: &Rc<RefCell<Inner>>, crtc: crtc::Handle, metadata: Option<&DrmEventMetadata>) {
    let mut inner = inner.borrow_mut();
    let Some(lit) = inner
        .gpu
        .as_mut()
        .and_then(|gpu| gpu.outputs.get_mut(&crtc))
    else {
        return;
    };
    match lit.surface.frame_submitted() {
        Ok(Some(Some(presentation))) => presentation.shown(vblank_shown(metadata)),
        Ok(_) => {}
        Err(error) => tracing::warn!(?error, output = lit.output.name(), "page flip not retired"),
    }
    lit.queued = false;
    lit.next_frame = Instant::now();
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
                vblank(&vblank_inner, crtc, metadata.as_ref());
                render_ready(&vblank_inner, &mut data.state);
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
            .filter(|capability| !matches!(capability, Capability::Fencing | Capability::ExportFence));
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
        let Some(crtc) = free_crtc(gpu.manager.device(), &resources, info, &used) else {
            tracing::warn!(output = name, "no free CRTC");
            continue;
        };
        let scale = config.scales.get(&name).copied().unwrap_or(1.0);
        if let Some(lit) = light(gpu, state, info, &name, crtc, *mode, scale) {
            used.insert(crtc);
            gpu.outputs.insert(crtc, lit);
        }
    }
    arrange_outputs(gpu, &config, state);
}

/// Place every lit output: where the configuration says, the rest packed
/// left to right in their current order after the configured ones, so an
/// unplugged display leaves no hole for the pointer to get lost in.
fn arrange_outputs(gpu: &Gpu, config: &DisplayConfig, state: &mut State) {
    let mut lit: Vec<&Output> = gpu.outputs.values().map(|lit| &lit.output).collect();
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
    let global = output.create_global::<State>(&state.display);
    state.add_output(&output, location);
    let interval = refresh_interval(output_mode.refresh);
    tracing::info!(output = name, ?interval, "output lit");
    Some(Lit {
        output,
        global,
        connector: info.handle(),
        modes: info.modes().to_vec(),
        surface,
        queued: false,
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
