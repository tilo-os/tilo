//! A bare TTY: KMS outputs, libinput devices, a libseat session.
//!
//! The session (logind or seatd) hands out the device descriptors and pauses
//! everything on a VT switch. Each connected connector of every GPU on the
//! seat becomes an output driven by smithay's `DrmCompositor`: planes,
//! direct scanout of client buffers and the cursor plane where the hardware
//! has them, GLES composition of the rest, one frame per vertical blank.
//! The primary GPU renders every output; a frame for another GPU's display
//! is copied over to it (smithay's `GpuManager`).
//! libinput's events become [`InputEvent`]s. Decisions that need no hardware
//! live in [`devices`] and [`seat_diag`], where they are tested.

pub mod access_keys;
mod color;
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
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType, VrrSupport,
};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::libinput::LibinputSessionInterface;
use smithay::backend::allocator::Format as DrmFormat;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::ImportDma as _;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder};
use smithay::backend::renderer::element::{RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session as _};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{
    Device as _, Mode as DrmMode, ModeFlags, ModeTypeFlags, connector, crtc,
};
use smithay::reexports::input::Libinput as LibinputContext;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Point, Transform};

use super::{Backend, BackendError, failed};
use crate::output::{ModeRequest, OutputChange, apply_placement, find_mode, refresh_interval};
use crate::pacing::Pacing;
use crate::present::{Presentation, Shown};
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};
use devices::{ModeInfo, select_mode};
use wake_check::{LitState, WakeCheck, WakeStage, WakeVerdict};

pub use crate::config::DisplayConfig;

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
/// What a queued frame carries to its vertical blank: its clients'
/// presentation feedback.
type Queued = Option<Presentation>;
type Manager = DrmOutputManager<Allocator, Exporter, Queued, DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, Queued, DrmDeviceFd>;
type Api = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
/// One GLES renderer per GPU.
type Renderers = GpuManager<Api>;
/// The primary GPU's renderer, drawing for one GPU's output.
type Renderer<'a> = MultiRenderer<'a, 'a, Api, Api>;

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
    color: LitColor,
    /// What clients on this output are told to allocate, when its planes
    /// can show their buffers directly.
    feedback: Option<ScanoutFeedback>,
    /// When the next frame may be drawn though nothing was queued.
    next_frame: Instant,
    interval: Duration,
    pacing: Pacing,
}

/// The dma-buf feedback for client surfaces on one output: the primary
/// GPU's render formats, or, for a surface whose buffer could go straight
/// onto a plane, the planes' formats first.
struct ScanoutFeedback {
    render: DmabufFeedback,
    scanout: DmabufFeedback,
}

impl ScanoutFeedback {
    /// The feedback for `surface`'s planes on the primary GPU (`primary`,
    /// its card `card`). A display on another GPU gets none: a buffer the
    /// primary rendered goes through a copy there anyway.
    fn new(
        surface: &Surface,
        primary: DrmNode,
        card: DrmNode,
        render_formats: &FormatSet,
    ) -> Option<Self> {
        let plane_formats: Vec<_> = surface.with_compositor(|compositor| {
            let planes = compositor.surface();
            planes
                .plane_info()
                .formats
                .iter()
                .chain(
                    planes
                        .planes()
                        .overlay
                        .iter()
                        .flat_map(|plane| plane.formats.iter()),
                )
                .filter(|format| render_formats.contains(format))
                .copied()
                .collect()
        });
        let builder = DmabufFeedbackBuilder::new(primary.dev_id(), render_formats.iter().copied());
        let render = builder.clone().build().ok()?;
        let scanout = builder
            .add_preference_tranche(card.dev_id(), Some(TrancheFlags::Scanout), plane_formats)
            .build()
            .ok()?;
        Some(Self { render, scanout })
    }
}

/// An output's colour pass and HDR signal.
#[derive(Default)]
struct LitColor {
    pass: Option<color::ColorPass>,
    /// The connector signals HDR.
    hdr_on: bool,
    /// What was asked last and could not be done, so it is not retried
    /// every frame.
    refused: Option<color::Wanted>,
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

/// One GPU: its display device and outputs.
struct Gpu {
    node: DrmNode,
    /// Its render node, which names its renderer.
    render: DrmNode,
    path: PathBuf,
    manager: Manager,
    outputs: HashMap<crtc::Handle, Lit>,
    token: RegistrationToken,
}

struct Inner {
    session: LibSeatSession,
    libinput: LibinputContext,
    /// The GPUs on the seat, by device number.
    gpus: HashMap<u64, Gpu>,
    renderers: Renderers,
    /// The render node of the GPU that renders every output.
    primary: Option<DrmNode>,
    config: DisplayConfig,
    active: bool,
    /// The last topology line written, so it is written when it changes.
    topology: String,
}

impl Inner {
    /// The GPU driving the output named `name`, and the output.
    fn lit_mut(&mut self, name: &str) -> Option<(&mut Gpu, crtc::Handle)> {
        self.gpus.values_mut().find_map(|gpu| {
            let crtc = gpu
                .outputs
                .iter()
                .find(|(_, lit)| lit.output.name() == name)
                .map(|(crtc, _)| *crtc)?;
            Some((gpu, crtc))
        })
    }
}

/// The renderer that draws for `target`'s outputs: the primary GPU's,
/// copying the frame over when `target` is another GPU.
fn renderer_for(
    renderers: &mut Renderers,
    primary: DrmNode,
    target: DrmNode,
    format: Fourcc,
) -> Result<Renderer<'_>, BackendError> {
    if target == primary {
        renderers.single_renderer(&primary)
    } else {
        renderers.renderer(&primary, &target, format)
    }
    .map_err(|error| failed("renderer", error))
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

        let renderers = Renderers::new(GbmGlesBackend::with_factory(gles_renderer))
            .map_err(|error| failed("renderers", error))?;
        let inner = Rc::new(RefCell::new(Inner {
            session,
            libinput: libinput.clone(),
            gpus: HashMap::new(),
            renderers,
            primary: None,
            config,
            active: true,
            topology: String::new(),
        }));

        let udev = UdevBackend::new(&seat).map_err(|error| failed("udev", error))?;
        let gpu_path = primary_gpu(&seat, &udev)?;
        drive_primary(handle, &inner, state, &gpu_path)?;
        // The other GPUs drive their displays; one that cannot be opened
        // leaves the rest working.
        let others =
            devices::filter_kms_nodes(udev.device_list().map(|(_, path)| path.to_path_buf()));
        for path in others.into_iter().filter(|path| *path != gpu_path) {
            add_gpu(handle, &inner, &path);
        }
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
                    input_source::Delivered::Pad {
                        device,
                        action,
                        mode,
                    } => {
                        libinput::dispatch_pad(&devices, &device, action, mode, &mut data.state);
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
                    // Another session may have left its own ramps.
                    data.state.gamma.forget_applied();
                    scan_connectors(&session_inner, &mut data.state);
                }
            })
            .map_err(|error| failed("session source", error.error))?;

        let udev_inner = Rc::clone(&inner);
        let udev_handle = handle.clone();
        handle
            .insert_source(udev, move |event, (), data| match event {
                UdevEvent::Changed { device_id } => {
                    if udev_inner.borrow().gpus.contains_key(&device_id) {
                        scan_connectors(&udev_inner, &mut data.state);
                    }
                }
                UdevEvent::Added { device_id, path } => {
                    if !udev_inner.borrow().gpus.contains_key(&device_id)
                        && !devices::filter_kms_nodes([path.clone()]).is_empty()
                    {
                        add_gpu(&udev_handle, &udev_inner, &path);
                        scan_connectors(&udev_inner, &mut data.state);
                    }
                }
                UdevEvent::Removed { device_id } => {
                    remove_gpu(&udev_inner, &udev_handle, &mut data.state, device_id);
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

    fn backdrop(
        &mut self,
        state: &mut State,
        region: (usize, usize),
        request: &crate::BackdropRequest,
    ) -> Result<Option<crate::BackdropImage>, BackendError> {
        let mut inner = self.inner.borrow_mut();
        if !inner.active {
            return Ok(None);
        }
        let primary = inner.primary.ok_or_else(|| BackendError("no GPU".into()))?;
        let mut renderer = inner
            .renderers
            .single_renderer(&primary)
            .map_err(|error| failed("renderer", error))?;
        crate::backdrop::draw::<_, smithay::backend::renderer::gles::GlesTexture>(
            &mut renderer,
            state,
            region,
            request,
        )
    }

    fn capture(
        &mut self,
        state: &mut State,
        frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        let mut inner = self.inner.borrow_mut();
        if !inner.active {
            // Another session has the GPU.
            return Err(BackendError("the session is not active".into()).into());
        }
        let primary = inner.primary.ok_or_else(|| BackendError("no GPU".into()))?;
        let mut renderer = inner
            .renderers
            .single_renderer(&primary)
            .map_err(|error| failed("renderer", error))?;
        crate::capture::copy::<_, smithay::backend::renderer::gles::GlesTexture>(
            &mut renderer,
            state,
            frame,
        )
    }

    fn configure(
        &mut self,
        state: &mut State,
        changes: &[OutputChange],
        test: bool,
    ) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        // Every mode is resolved before anything changes: all or nothing.
        let mut plan = Vec::with_capacity(changes.len());
        for change in changes {
            let (gpu, crtc) = inner
                .lit_mut(&change.name)
                .ok_or_else(|| BackendError(format!("{}: not lit", change.name)))?;
            let Some(lit) = gpu.outputs.get(&crtc) else {
                continue;
            };
            let mode = match change.mode {
                Some(request) => Some(drm_mode(&lit.modes, request).ok_or_else(|| {
                    BackendError(format!("{}: the display has no such mode", change.name))
                })?),
                None => None,
            };
            plan.push((gpu.node.dev_id(), crtc, mode, change));
        }
        if test {
            return Ok(());
        }
        let Inner {
            gpus, renderers, ..
        } = &mut *inner;
        for (device, crtc, mode, change) in plan {
            let Some(gpu) = gpus.get_mut(&device) else {
                continue;
            };
            let Some(lit) = gpu.outputs.get_mut(&crtc) else {
                continue;
            };
            if let Some(mode) = mode {
                let mut renderer = renderers
                    .single_renderer(&gpu.render)
                    .map_err(|error| failed("renderer", error))?;
                lit.surface
                    .use_mode::<_, OutputRenderElement<Renderer<'_>>>(
                        mode,
                        &mut renderer,
                        &DrmOutputRenderElements::default(),
                    )
                    .map_err(|error| failed(&format!("{}: mode", change.name), error))?;
                let output_mode = OutputMode::from(mode);
                lit.output
                    .change_current_state(Some(output_mode), None, None, None);
                lit.interval = refresh_interval(output_mode.refresh);
            }
            apply_placement(state, &lit.output, change);
            // The colour pass is sized for the old mode, scale and rotation.
            lit.color.pass = None;
            lit.color.refused = None;
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
        log_topology(&mut inner, state);
        Ok(())
    }

    fn set_power(&mut self, state: &mut State, name: &str, on: bool) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        // A configuration that turned an output off or on changes what is
        // driven.
        log_topology(&mut inner, state);
        let (gpu, crtc) = inner
            .lit_mut(name)
            .ok_or_else(|| BackendError(format!("{name}: not lit")))?;
        let Some(lit) = gpu.outputs.get_mut(&crtc) else {
            return Ok(());
        };
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

    /// Through the CRTC's legacy gamma ioctl, which atomic drivers turn
    /// into their `GAMMA_LUT`: the display engine applies it after the
    /// planes are blended, so it holds for directly scanned-out buffers too.
    fn set_gamma(
        &mut self,
        _state: &mut State,
        name: &str,
        ramp: Option<&crate::gamma::Ramp>,
    ) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        let (gpu, crtc) = inner
            .lit_mut(name)
            .ok_or_else(|| BackendError(format!("{name}: not lit")))?;
        let device = gpu.manager.device();
        let identity;
        let ramp = if let Some(ramp) = ramp {
            ramp
        } else {
            identity = crate::gamma::Ramp::identity(gamma_size(device, crtc).unwrap_or(0));
            &identity
        };
        device
            .set_gamma(crtc, &ramp.red, &ramp.green, &ramp.blue)
            .map_err(|error| failed(&format!("{name}: gamma ramp"), error))
    }

    fn set_display_config(&mut self, config: &DisplayConfig) {
        self.inner.borrow_mut().config = config.clone();
    }

    fn set_input_config(&mut self, state: &mut State, config: input_config::InputConfig) {
        self.input.borrow_mut().set_config(config, state);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let inner = self.inner.borrow();
        inner
            .gpus
            .values()
            .flat_map(|gpu| gpu.outputs.values())
            .filter(|lit| !lit.queued && !lit.dark)
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
    let Inner {
        gpus,
        renderers,
        primary,
        config,
        ..
    } = &mut *inner;
    let Some(primary) = *primary else {
        return;
    };
    let now = Instant::now();
    for gpu in gpus.values_mut() {
        render_gpu(gpu, renderers, primary, config, state, now);
    }
}

/// Draw `gpu`'s outputs that are due.
fn render_gpu(
    gpu: &mut Gpu,
    renderers: &mut Renderers,
    primary: DrmNode,
    config: &DisplayConfig,
    state: &State,
    now: Instant,
) {
    let Gpu {
        manager,
        outputs,
        render,
        ..
    } = gpu;
    let device = manager.device();
    for lit in outputs.values_mut() {
        if lit.queued
            && lit.wake.as_ref().is_some_and(|wake| !wake.judged)
            && now.saturating_duration_since(lit.queued_at) > LINK_WAIT
        {
            // No vertical blank after the modeset: the link did not come up.
            judge_wake(
                lit,
                device,
                &**state.idle.dimmer.backlight(),
                Some(WakeStage::Link),
            );
        }
        if lit.queued || lit.dark || now < lit.next_frame {
            continue;
        }
        // The build starts at its deadline; a wakeup a refresh or more
        // past it is off the grid, and starts it.
        let start = if now.saturating_duration_since(lit.next_frame) < lit.interval {
            lit.next_frame
        } else {
            now
        };
        let name = lit.output.name();
        lit.follow_vrr(vrr_wanted(
            lit.vrr != Vrr::Unable,
            config.variable_refresh.contains(&name),
            state.fullscreen_on(&lit.output),
        ));
        let mut renderer = match renderer_for(renderers, primary, *render, lit.surface.format()) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::warn!(%error, output = name, "no renderer");
                continue;
            }
        };
        let wanted = color::Wanted {
            profile: config.color_profiles.get(&name).cloned(),
            hdr: config.hdr.get(&name).cloned(),
        };
        // An output another GPU shows gets a copy in its swapchain's
        // format; only one the primary draws directly changes depth.
        let depth = (*render == primary).then(|| manager.allocator());
        lit.follow_color(wanted, &mut renderer, device, depth);
        // The pointer in front, where DrmCompositor can give it the cursor
        // plane.
        let mut elements: Vec<OutputRenderElement<Renderer<'_>>> =
            state.cursor_elements(&mut renderer, &lit.output);
        elements.extend(output_elements(&mut renderer, state, &lit.output));
        match lit.color.pass.take() {
            Some(mut pass) => {
                match pass.render(&mut renderer, &elements) {
                    Ok(states) => {
                        submit(
                            lit,
                            &mut renderer,
                            &[&pass],
                            Some(&states),
                            state,
                            device,
                            now,
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%error, output = name, "colour pass skipped");
                        submit(lit, &mut renderer, &elements, None, state, device, now);
                    }
                }
                lit.color.pass = Some(pass);
            }
            None => submit(lit, &mut renderer, &elements, None, state, device, now),
        }
        if lit.queued {
            lit.pacing.queued(start, Instant::now(), lit.interval);
        } else {
            lit.next_frame = lit.pacing.idle(now, lit.interval, lit.vrr == Vrr::On);
        }
    }
}

/// Compose `elements` onto `lit` and queue the frame. `states`: where the
/// clients' surfaces were drawn, when that was an offscreen pass rather
/// than this composition.
fn submit<'r, E: RenderElement<Renderer<'r>>>(
    lit: &mut Lit,
    renderer: &mut Renderer<'r>,
    elements: &[E],
    states: Option<&RenderElementStates>,
    state: &State,
    device: &DrmDevice,
    now: Instant,
) {
    match lit
        .surface
        .render_frame(renderer, elements, BACKGROUND, FrameFlags::DEFAULT)
    {
        Ok(frame) => {
            // Clients draw their next frame now; an output with nothing
            // new is asked again one refresh later.
            let states = states.unwrap_or(&frame.states);
            if let Some(feedback) = &lit.feedback {
                state.send_scanout_feedback(
                    &lit.output,
                    states,
                    &feedback.render,
                    &feedback.scanout,
                );
            }
            let presentation = state.frame_drawn(&lit.output, Some(states));
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
                                device,
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
}

fn vblank(
    inner: &Rc<RefCell<Inner>>,
    state: &State,
    device: u64,
    crtc: crtc::Handle,
    metadata: Option<&DrmEventMetadata>,
) {
    let mut inner = inner.borrow_mut();
    let Some(gpu) = inner.gpus.get_mut(&device) else {
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
    let shown = vblank_instant(metadata);
    lit.next_frame = lit.pacing.shown(shown, lit.interval, lit.vrr == Vrr::On);
    if let Some(summary) = lit.pacing.take_summary(shown, lit.interval) {
        tracing::debug!(
            output = lit.output.name(),
            shown = summary.shown,
            missed = summary.missed,
            interval_us = lit.interval.as_micros(),
            lead_us = summary.lead_us,
            cost_p50_us = summary.cost.0,
            cost_p99_us = summary.cost.1,
            build_to_vblank_p50_us = summary.build_to_vblank.0,
            build_to_vblank_p99_us = summary.build_to_vblank.1,
            "frame pacing"
        );
    }
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
    /// Bring the output's colour pass, HDR signal and swapchain depth to
    /// `wanted`; `depth` is the allocator for a swapchain of another depth,
    /// `None` keeps the depth.
    fn follow_color(
        &mut self,
        mut wanted: color::Wanted,
        renderer: &mut Renderer<'_>,
        device: &DrmDevice,
        depth: Option<&Allocator>,
    ) {
        let current = self.color.pass.as_ref().map(color::ColorPass::wanted);
        if current.map_or_else(|| wanted.plain(), |current| *current == wanted) {
            return;
        }
        if self.color.refused.as_ref() == Some(&wanted) {
            return;
        }
        let name = self.output.name();
        if wanted.hdr.is_some() || self.color.hdr_on {
            match color::signal_hdr(device, self.connector, wanted.hdr.as_ref()) {
                Ok(()) => self.color.hdr_on = wanted.hdr.is_some(),
                Err(error) => {
                    tracing::warn!(%error, output = name, "HDR signal not switched");
                    self.color.refused = Some(wanted.clone());
                    wanted.hdr = None;
                }
            }
        }
        self.color.pass = if wanted.plain() {
            None
        } else {
            match color::ColorPass::new(renderer, &self.output, wanted.clone()) {
                Ok(pass) => Some(pass),
                Err(error) => {
                    tracing::warn!(%error, output = name, "colour pass not set up");
                    self.color.refused = Some(wanted);
                    None
                }
            }
        };
        if let Some(allocator) = depth {
            self.follow_depth(renderer, allocator);
        }
        // A whole frame, through the new pass.
        self.surface.reset_buffers();
    }

    /// Ten bits a channel while the output signals HDR, eight otherwise:
    /// the first format of [`color::depth_formats`] the primary plane takes.
    fn follow_depth(&self, renderer: &mut Renderer<'_>, allocator: &Allocator) {
        let gles: &mut GlesRenderer = renderer.as_mut();
        let renderable: Vec<DrmFormat> = gles
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        let current = self.surface.format();
        let candidates = color::depth_formats(self.color.hdr_on, current, |code| {
            renderable.iter().any(|format| format.code == code)
        });
        let name = self.output.name();
        for code in candidates {
            let modifiers = renderable
                .iter()
                .filter(|format| format.code == code)
                .map(|format| format.modifier);
            match self.surface.with_compositor(|compositor| {
                compositor.set_format(allocator.clone(), code, modifiers)
            }) {
                Ok(()) => {
                    tracing::info!(output = name, ?code, "swapchain format");
                    return;
                }
                Err(error) => {
                    tracing::debug!(?error, output = name, ?code, "swapchain format refused");
                }
            }
        }
        if self.color.hdr_on && !color::HDR_FORMATS.contains(&current) {
            tracing::warn!(output = name, ?current, "HDR without a 10-bit swapchain");
        }
    }

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

/// The entries of `crtc`'s gamma ramp; `None` when it has none.
fn gamma_size(device: &DrmDevice, crtc: crtc::Handle) -> Option<usize> {
    let info = device.get_crtc(crtc).ok()?;
    usize::try_from(info.gamma_length())
        .ok()
        .filter(|size| *size >= 2)
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

/// When a vertical blank happened: the kernel's monotonic stamp, or now
/// when it gave none.
fn vblank_instant(metadata: Option<&DrmEventMetadata>) -> Instant {
    let now = Instant::now();
    let Some(DrmEventMetadata {
        time: DrmEventTime::Monotonic(time),
        ..
    }) = metadata
    else {
        return now;
    };
    let clock = smithay::reexports::rustix::time::clock_gettime(
        smithay::reexports::rustix::time::ClockId::Monotonic,
    );
    let clock = Duration::new(
        u64::try_from(clock.tv_sec).unwrap_or(0),
        u32::try_from(clock.tv_nsec).unwrap_or(0),
    );
    now.checked_sub(clock.saturating_sub(*time)).unwrap_or(now)
}

fn pause(inner: &Rc<RefCell<Inner>>) {
    let mut inner = inner.borrow_mut();
    inner.active = false;
    inner.libinput.suspend();
    for gpu in inner.gpus.values_mut() {
        gpu.manager.pause();
    }
}

fn resume(inner: &Rc<RefCell<Inner>>) {
    let mut inner = inner.borrow_mut();
    if inner.libinput.resume().is_err() {
        tracing::warn!("libinput did not resume");
    }
    for gpu in inner.gpus.values_mut() {
        if let Err(error) = gpu.manager.activate(false) {
            tracing::warn!(?error, path = %gpu.path.display(), "GPU did not reactivate");
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

/// Open the primary GPU, which renders every output, and offer its formats
/// to clients.
fn drive_primary(
    handle: &LoopHandle<'static, LoopData>,
    inner: &Rc<RefCell<Inner>>,
    state: &mut State,
    path: &Path,
) -> Result<(), BackendError> {
    let primary = open_gpu(handle, inner, path)?;
    let mut inner = inner.borrow_mut();
    inner.primary = Some(primary.render);
    let formats = inner
        .renderers
        .single_renderer(&primary.render)
        .map_err(|error| failed("renderer", error))?
        .dmabuf_formats()
        .iter()
        .copied()
        .collect();
    state.enable_dmabuf(formats, Some(primary.node));
    let capture_formats: Vec<_> = inner
        .renderers
        .single_renderer(&primary.render)
        .map_err(|error| failed("renderer", error))?
        .as_mut()
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect();
    state.enable_dmabuf_capture(primary.node.dev_id(), capture_formats);
    inner.gpus.insert(primary.node.dev_id(), primary);
    Ok(())
}

/// Open the GPU at `path` and drive its displays, rendered by the primary.
fn add_gpu(handle: &LoopHandle<'static, LoopData>, inner: &Rc<RefCell<Inner>>, path: &Path) {
    match open_gpu(handle, inner, path) {
        Ok(gpu) => {
            tracing::info!(path = %path.display(), "GPU added");
            inner.borrow_mut().gpus.insert(gpu.node.dev_id(), gpu);
        }
        Err(error) => tracing::warn!(%error, path = %path.display(), "GPU not driven"),
    }
}

fn remove_gpu(
    inner: &Rc<RefCell<Inner>>,
    handle: &LoopHandle<'static, LoopData>,
    state: &mut State,
    device: u64,
) {
    let mut inner = inner.borrow_mut();
    let Some(gpu) = inner.gpus.remove(&device) else {
        return;
    };
    tracing::warn!(path = %gpu.path.display(), "GPU removed");
    for lit in gpu.outputs.values() {
        state.remove_output(&lit.output);
    }
    handle.remove(gpu.token);
    inner.renderers.as_mut().remove_node(&gpu.render);
    if inner.primary == Some(gpu.render) {
        // Another GPU renders from now on.
        inner.primary = inner.gpus.values().next().map(|gpu| gpu.render);
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
    let render = node
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node);
    let (device, notifier) =
        DrmDevice::new(fd.clone(), true).map_err(|error| failed("DRM device", error))?;
    let gbm = GbmDevice::new(fd).map_err(|error| failed("GBM device", error))?;
    let renderer_formats = {
        let mut inner = inner.borrow_mut();
        inner
            .renderers
            .as_mut()
            .add_node(render, gbm.clone())
            .map_err(|error| failed("EGL display", error))?;
        inner
            .renderers
            .single_renderer(&render)
            .map_err(|error| failed("renderer", error))?
            .dmabuf_formats()
            .iter()
            .copied()
            .collect::<Vec<_>>()
    };
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(render));
    let manager = Manager::new(
        device,
        allocator,
        exporter,
        Some(gbm),
        color::SDR_FORMATS,
        renderer_formats,
    );

    let vblank_inner = Rc::clone(inner);
    let dev_id = node.dev_id();
    let token = handle
        .insert_source(notifier, move |event, metadata, data| match event {
            DrmEvent::VBlank(crtc) => {
                vblank(&vblank_inner, &data.state, dev_id, crtc, metadata.as_ref());
                render_ready(&vblank_inner, &data.state);
            }
            DrmEvent::Error(error) => tracing::warn!(?error, "DRM device error"),
        })
        .map_err(|error| failed("DRM source", error.error))?;
    Ok(Gpu {
        node,
        render,
        path: path.to_path_buf(),
        manager,
        outputs: HashMap::new(),
        token,
    })
}

/// A GPU's renderer. On panfrost (Mali) it runs without per-texture
/// fences: such a fence makes the driver submit every draw as its own
/// batch, each reloading and writing back the whole framebuffer; KMS waits
/// for the frame through the scanout buffer's implicit fence instead.
fn gles_renderer(
    display: &EGLDisplay,
) -> Result<GlesRenderer, smithay::backend::renderer::multigpu::gbm::Error> {
    let panfrost = EGLDevice::device_for_display(display)
        .and_then(|device| device.drm_device_path())
        .ok()
        .and_then(|path| driver_of(&path))
        .is_some_and(|driver| driver == "panfrost");
    let context = EGLContext::new(display)?;
    // SAFETY: the context is current only on this thread, the loop's.
    let renderer = if panfrost {
        let capabilities = unsafe { GlesRenderer::supported_capabilities(&context) }?
            .into_iter()
            .filter(|capability| {
                !matches!(capability, Capability::Fencing | Capability::ExportFence)
            });
        unsafe { GlesRenderer::with_capabilities(context, capabilities) }
    } else {
        unsafe { GlesRenderer::new(context) }
    };
    Ok(renderer?)
}

/// The kernel driver behind a DRM device node (`/dev/dri/card0`).
fn driver_of(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    let driver =
        std::fs::read_link(Path::new("/sys/class/drm").join(name).join("device/driver")).ok()?;
    Some(driver.file_name()?.to_string_lossy().into_owned())
}

/// Light connectors that became connected, drop those that went away, on
/// every GPU.
fn scan_connectors(inner: &Rc<RefCell<Inner>>, state: &mut State) {
    let mut inner = inner.borrow_mut();
    let Inner {
        gpus,
        renderers,
        config,
        primary,
        ..
    } = &mut *inner;
    let Some(primary) = *primary else {
        return;
    };
    let devices: Vec<u64> = gpus.keys().copied().collect();
    for device in devices {
        // Names other GPUs' outputs already have: a connector of the same
        // name here is told apart by its card.
        let taken: HashSet<String> = gpus
            .iter()
            .filter(|(id, _)| **id != device)
            .flat_map(|(_, gpu)| gpu.outputs.values().map(|lit| lit.output.name()))
            .collect();
        if let Some(gpu) = gpus.get_mut(&device) {
            scan_gpu(gpu, primary, renderers, config, &taken, state);
        }
    }
    arrange_outputs(gpus, config, state);
    log_topology(&mut inner, state);
}

/// The output name of a connector of the card at `path`: the connector's
/// name (`HDMI-A-1`), or `card1/HDMI-A-1` when another GPU's output has it.
fn output_name(path: &Path, connector: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(connector) {
        return connector.to_owned();
    }
    let card = path
        .file_name()
        .map_or_else(|| "card".into(), |name| name.to_string_lossy());
    format!("{card}/{connector}")
}

fn scan_gpu(
    gpu: &mut Gpu,
    primary: DrmNode,
    renderers: &mut Renderers,
    config: &DisplayConfig,
    taken: &HashSet<String>,
    state: &mut State,
) {
    let device = gpu.manager.device();
    let Ok(resources) = device.resource_handles() else {
        tracing::warn!(path = %gpu.path.display(), "DRM resources unreadable");
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
        let connector_name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let name = output_name(&gpu.path, &connector_name, taken);
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
        let path = gpu.path.clone();
        let attempt = |result| {
            tracing::info!(
                "{}",
                topology::connector_attempt_line(&path, &name, mode.size(), result)
            );
        };
        let Some(crtc) = free_crtc(gpu.manager.device(), &resources, info, &used) else {
            attempt(topology::ConnectorAttempt::Failed(
                "no free CRTC can drive this connector",
            ));
            continue;
        };
        let scale = config.scales.get(&name).copied().unwrap_or(1.0);
        let lit = light(
            gpu,
            renderers,
            state,
            &Lighting {
                primary,
                info,
                name: &name,
                crtc,
                mode: *mode,
                scale,
            },
        );
        attempt(if lit.is_some() {
            topology::ConnectorAttempt::Lit
        } else {
            topology::ConnectorAttempt::Failed("the modeset was refused")
        });
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
}

/// Write the topology line when what is driven changed.
fn log_topology(inner: &mut Inner, state: &State) {
    let cards: std::collections::BTreeMap<u64, PathBuf> = inner
        .gpus
        .values()
        .map(|gpu| (gpu.node.dev_id(), gpu.path.clone()))
        .collect();
    let mut driven: Vec<topology::Driven> = inner
        .gpus
        .values()
        .flat_map(|gpu| gpu.outputs.values().map(move |lit| (gpu, lit)))
        .filter(|(_, lit)| !state.disabled_outputs.contains(&lit.output))
        .map(|(gpu, lit)| {
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
    if line != inner.topology {
        tracing::info!("{line}");
        inner.topology = line;
    }
}

/// Place every lit output: where the configuration says, the rest packed
/// left to right in their current order after the configured ones, so an
/// unplugged display leaves no hole for the pointer to get lost in.
fn arrange_outputs(gpus: &HashMap<u64, Gpu>, config: &DisplayConfig, state: &mut State) {
    // Outputs a configuration turned off keep their place for when they
    // come back but take none in the layout.
    let mut lit: Vec<&Output> = gpus
        .values()
        .flat_map(|gpu| gpu.outputs.values())
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
/// A connector to light: its name, the CRTC and mode to drive it with and
/// the scale it starts at.
struct Lighting<'a> {
    /// The render node of the GPU that renders every output.
    primary: DrmNode,
    info: &'a connector::Info,
    name: &'a str,
    crtc: crtc::Handle,
    mode: DrmMode,
    scale: f64,
}

fn light(
    gpu: &mut Gpu,
    renderers: &mut Renderers,
    state: &mut State,
    lighting: &Lighting<'_>,
) -> Option<Lit> {
    let &Lighting {
        primary,
        info,
        name,
        crtc,
        mode,
        scale,
    } = lighting;
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
    let mut renderer = renderers
        .single_renderer(&gpu.render)
        .map_err(|error| tracing::warn!(?error, output = name, "no renderer"))
        .ok()?;
    let surface = gpu
        .manager
        .initialize_output::<_, OutputRenderElement<Renderer<'_>>>(
            crtc,
            mode,
            &[info.handle()],
            &output,
            None,
            &mut renderer,
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
    let feedback = if gpu.render == primary {
        renderers
            .single_renderer(&primary)
            .ok()
            .and_then(|renderer| {
                ScanoutFeedback::new(&surface, primary, gpu.node, &renderer.dmabuf_formats())
            })
    } else {
        None
    };
    crate::output::advertise(&output, &state.display);
    state.add_output(&output, location);
    state
        .gamma
        .set_size(name, gamma_size(gpu.manager.device(), crtc));
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
        color: LitColor::default(),
        feedback,
        next_frame: Instant::now(),
        interval,
        pacing: Pacing::default(),
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

    #[test]
    fn a_connector_name_another_gpu_has_is_qualified_by_its_card() {
        let taken = HashSet::from(["HDMI-A-1".to_owned()]);
        let card = Path::new("/dev/dri/card1");
        assert_eq!(output_name(card, "DP-1", &taken), "DP-1");
        assert_eq!(output_name(card, "HDMI-A-1", &taken), "card1/HDMI-A-1");
    }
}
