//! One page-lifetime winit loop. JavaScript owns the DOM and `IndexedDB`; this
//! owns the real application component, its frame pipeline and the GPU.
//!
//! Records and views live in `durable_store::MemoryStore` via
//! [`crate::storage`]. A mount is the session desktop ([`crate::desktop`])
//! unless the page names one application.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use js_sys::{Function, Reflect, Uint8Array};
use ui::application::{
    ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOwners, RecordStore};
use wasm_bindgen::{JsCast, prelude::*};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::web::{EventLoopExtWebSys as _, WindowAttributesExtWebSys as _};
use winit::window::{Window, WindowId};

use crate::viewport::{SurfaceCaps, Viewport, ViewportState};

thread_local! {
    static LOOP: RefCell<Option<EventLoopProxy<u64>>> = const { RefCell::new(None) };
    static MOUNTS: RefCell<HashMap<u64, Mount>> = RefCell::new(HashMap::new());
    static REQUESTS: RefCell<HashMap<u64, Request>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u64> = const { RefCell::new(1) };
    static STOP: RefCell<Option<Function>> = const { RefCell::new(None) };
    static SHARED_GPU: RefCell<Option<Rc<RefCell<SharedGpu>>>> = const { RefCell::new(None) };
    /// Mount GPU setups, run one at a time: opening a workspace while another
    /// sets up must not stack a second device creation on the first.
    static GPU_SETUPS: RefCell<std::collections::VecDeque<GpuSetup>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
    static GPU_SETUP_RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    /// One application to mount on its own; absent (or `desktop`) mounts
    /// the session desktop.
    #[serde(default)]
    app: Option<String>,
    /// The desktop's initial windows, one per application id; every
    /// registered application when absent.
    #[serde(default)]
    apps: Option<Vec<String>>,
    profile: String,
    device: String,
    room: String,
    /// Who edits; the local identity's person (a certified peer's user)
    /// when absent.
    #[serde(default)]
    author: Option<String>,
    instance: u64,
    /// The record to open; absent opens the instance's default record,
    /// created on first use.
    record: Option<String>,
    /// Create a record even when the instance has a default one.
    #[serde(default, rename = "newDocument")]
    new_document: bool,
    #[serde(default)]
    mac: bool,
    /// A started certified peer (`WasmPeer.id`) whose room this mount's
    /// records and desktop replicate through.
    #[serde(default)]
    peer: Option<u32>,
    /// Milliseconds between semantics deliveries while the tree keeps
    /// changing without a focus or structure change.
    #[serde(default)]
    semantics_window_ms: Option<u64>,
    /// Render within WebKit's GPU memory: a smaller canvas backing store and
    /// scratch budget (`SurfaceCaps::CONSERVATIVE`).
    #[serde(default)]
    conservative_gpu: bool,
}

impl Config {
    fn author(&self) -> String {
        self.author
            .clone()
            .unwrap_or_else(|| ui::collaboration::local_user_id().0)
    }
}

struct Request {
    canvas: web_sys::HtmlCanvasElement,
    config: Config,
    fonts: JsValue,
    hydrated: JsValue,
    status: Function,
}

/// The page's one WebGPU device and vello renderer, shared by every mount.
///
/// A device per mount cost every opened workspace a second adapter, device
/// and full pipeline compilation while the first stayed resident. In WebKit
/// each device holds ~140 MB of its own and compiling vello's pipelines
/// peaks near 200 MB, under a GPU-process limit (600 MB) that Safari's other
/// tabs share.
struct SharedGpu {
    cx: ui::gpu::RenderContext,
    renderer: vello::Renderer,
    failure: Arc<Mutex<Option<String>>>,
    /// Mounts rendering with this device; a loss or error wakes each.
    mounts: Arc<Mutex<Vec<u64>>>,
}

impl SharedGpu {
    fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn detach(&self, id: u64) -> bool {
        let mut mounts = self.mounts.lock().unwrap_or_else(PoisonError::into_inner);
        mounts.retain(|&m| m != id);
        mounts.is_empty()
    }
}

/// One mount's surface on the shared device.
struct Gpu {
    shared: Rc<RefCell<SharedGpu>>,
    surface: vello::util::RenderSurface<'static>,
}

impl Gpu {
    fn failure(&self) -> Option<String> {
        self.shared.try_borrow().ok().and_then(|s| s.failure())
    }
}

/// Stop rendering `id` with the shared device, and release the device once
/// no mount renders with it or it failed.
fn release_gpu(id: u64, gpu: Gpu) {
    // On WebGPU a dropped texture stays resident until JavaScript collects it.
    gpu.surface.target_texture.destroy();
    let (last, failed) = gpu
        .shared
        .try_borrow()
        .map_or((false, false), |s| (s.detach(id), s.failure().is_some()));
    if last || failed {
        SHARED_GPU.with_borrow_mut(|shared| {
            if shared.as_ref().is_some_and(|s| Rc::ptr_eq(s, &gpu.shared)) {
                *shared = None;
            }
        });
    }
}

struct GpuSetup {
    id: u64,
    generation: u64,
    window: Arc<Window>,
    caps: SurfaceCaps,
}

struct Mount {
    metrics: RefCell<crate::frame_metrics::FrameMetrics>,
    /// Which statuses carry the semantics tree: unchanged trees are omitted,
    /// content-only changes are throttled, focus and structure changes go out
    /// at once.
    semantics: RefCell<crate::semantics_status::SemanticsGate>,
    window: Arc<Window>,
    canvas: web_sys::HtmlCanvasElement,
    frame: FrameHarness,
    store: Rc<durable_store::MemoryStore>,
    /// Every package record owner the mount opened, flushed together.
    stores: Rc<RefCell<Vec<RecordStore>>>,
    /// The single application's record; `None` for the desktop.
    checkout: Option<RecordCheckout>,
    desktop: Option<Desktop>,
    gpu: Option<Gpu>,
    pending: bool,
    viewport: ViewportState,
    caps: SurfaceCaps,
    hidden: bool,
    generation: u64,
    frames: u64,
    status: Function,
    images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    error: Option<String>,
    /// The last composed scene exceeded `scene_within_limits` (reported once).
    oversized_scene: bool,
    /// The peer this mount replicates through, and whether a peer's change
    /// reached it since the last frame.
    peer: Option<u32>,
    replica_changed: Rc<std::cell::Cell<bool>>,
    /// The focus presence last reported to the page.
    focus_presence: RefCell<Option<serde_json::Value>>,
}

struct Desktop {
    handle: tilo_desktop_surface::DesktopHandle,
    shared: crate::desktop::Shared,
    persistence: crate::desktop::Persistence,
    /// Navigation commands name the mount generation they were issued for.
    generation: String,
}

impl Mount {
    fn flush_records(&self) {
        for store in self.stores.borrow().iter() {
            let _ = store.try_flush();
        }
        self.save_desktop();
    }

    /// Keep the room's desktop in the profile after anything moved it.
    fn save_desktop(&self) {
        if let Some(desktop) = &self.desktop
            && let Err(error) = desktop.persistence.save(&desktop.handle)
        {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "Browser runtime: desktop not saved: {error}"
            )));
        }
    }
}

/// Glyphs one frame may encode. A full 4K screen of small text is far below
/// this; only unculled off-screen content reaches it.
const MAX_SCENE_GLYPHS: usize = 400_000;
/// Encoded scene bytes one frame may hand to the renderer.
const MAX_SCENE_BYTES: usize = 128 << 20;
/// vello's adaptive scratch buffers, in total, above the viewport floors. A
/// 60-window desktop at the capped canvas holds about 92 MiB of them, floors
/// included. Safari kills its GPU process past 600 MB; with every buffer
/// allowed a full 128 MiB binding, the renderer alone could reach 900 MB.
const MAX_BUMP_BUDGET_BYTES: u64 = 128 << 20;
/// The scratch budget under `conservative_gpu`.
const CONSERVATIVE_BUMP_BUDGET_BYTES: u64 = 32 << 20;
/// The image atlas's side under `conservative_gpu`: 64 MB instead of vello's
/// 256 MB maximum, which opening a photo-heavy app could reach in one frame.
const CONSERVATIVE_IMAGE_ATLAS_MAX: u32 = 4096;

fn scene_within_limits(scene: &vello::Scene) -> Result<(), String> {
    let encoding = scene.encoding();
    let glyphs = encoding.resources.glyphs.len();
    let bytes = encoding
        .path_tags
        .len()
        .saturating_add(encoding.path_data.len().saturating_mul(4))
        .saturating_add(encoding.draw_tags.len().saturating_mul(4))
        .saturating_add(encoding.draw_data.len().saturating_mul(4))
        .saturating_add(encoding.transforms.len().saturating_mul(24))
        .saturating_add(encoding.styles.len().saturating_mul(8));
    if glyphs > MAX_SCENE_GLYPHS || bytes > MAX_SCENE_BYTES {
        Err(format!(
            "Browser runtime: frame not rendered, scene too large ({glyphs} glyphs, {bytes} encoded bytes)"
        ))
    } else {
        Ok(())
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a map_err adapter receives each error by value"
)]
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn emit(f: &Function, v: &serde_json::Value) {
    emit_text(f, &v.to_string());
}

fn emit_text(f: &Function, text: &str) {
    let _ = f.call1(&JsValue::NULL, &JsValue::from_str(text));
}

fn proxy() -> Result<EventLoopProxy<u64>, JsValue> {
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|panic| {
            // Only the first: later panics follow from the state it left
            // borrowed, and would bury it.
            if !crate::crash::report(panic) {
                return;
            }
            // A JS Error carries the wasm call stack, which names the caller
            // a std panic location does not.
            web_sys::console::error_1(&js_sys::Error::new(&format!(
                "Browser runtime panic: {panic}"
            )));
        }));
    });
    if let Some(p) = LOOP.with_borrow(Clone::clone) {
        return Ok(p);
    }
    crate::host_services::install();
    let event_loop = EventLoop::<u64>::with_user_event().build().map_err(error)?;
    let proxy = event_loop.create_proxy();
    LOOP.with_borrow_mut(|p| *p = Some(proxy.clone()));
    event_loop.spawn_app(Runner);
    Ok(proxy)
}

fn with_mount<R>(
    id: u64,
    run: impl FnOnce(&mut Mount) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
    // After a panic the module keeps whatever the panic left borrowed; a
    // later call from the page fails instead of panicking again.
    if crate::crash::stopped() {
        return Err(error(crate::crash::STOPPED));
    }
    MOUNTS.with(|mounts| {
        let mut mounts = mounts
            .try_borrow_mut()
            .map_err(|_| error("the mount is busy (a call from inside the runtime)"))?;
        run(mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?)
    })
}

/// Mount an application on `canvas`. `config` is JSON (`app`, `profile`,
/// `device`, `room`, `author`, `instance`, optional `record`, `mac`);
/// `fonts` maps startup font file names to bytes; `hydrated` holds the
/// namespace's stored rows. Statuses arrive on `status` as JSON text.
///
/// # Errors
///
/// Rejects during terminal shutdown or for malformed configuration.
#[wasm_bindgen]
pub fn mount(
    canvas: web_sys::HtmlCanvasElement,
    config: &str,
    fonts: JsValue,
    hydrated: JsValue,
    status: Function,
) -> Result<u64, JsValue> {
    if crate::crash::stopped() {
        return Err(error(crate::crash::STOPPED));
    }
    if STOP.with_borrow(Option::is_some) {
        return Err(error("terminal shutdown is still pending"));
    }
    let p = proxy()?;
    let id = NEXT.with_borrow_mut(|next| {
        let id = *next;
        *next = next.saturating_add(1);
        id
    });
    let config = serde_json::from_str(config).map_err(error)?;
    REQUESTS.with_borrow_mut(|r| {
        r.insert(
            id,
            Request {
                canvas,
                config,
                fonts,
                hydrated,
                status,
            },
        )
    });
    p.send_event(id).map_err(error)?;
    Ok(id)
}

/// Close one mount and return its remaining writes (`[{key, bytes, actor,
/// revision}]`) for the page to commit and acknowledge.
///
/// # Errors
///
/// Propagates a failure to serialize the remaining writes.
#[wasm_bindgen]
pub fn close(id: u64) -> Result<JsValue, JsValue> {
    REQUESTS.with_borrow_mut(|r| r.remove(&id));
    let Some(mut mount) = MOUNTS.with_borrow_mut(|m| m.remove(&id)) else {
        return Ok(js_sys::Array::new().into());
    };
    if let Some(peer) = mount.peer {
        crate::replication::detach(peer, id);
    }
    mount.frame.close_components();
    let reports = mount.frame.take_component_close_reports();
    mount.flush_records();
    let writes = crate::storage::drain(&mount.store)?;
    emit(
        &mount.status,
        &serde_json::json!({"phase":"closed","id":id,"closeReports":reports.len(),"pendingWrites":js_sys::Array::from(&writes).length()}),
    );
    unconfigure(&mount.canvas);
    if let Some(gpu) = mount.gpu.take() {
        release_gpu(id, gpu);
    }
    Ok(writes)
}

fn unconfigure(canvas: &web_sys::HtmlCanvasElement) {
    if let Ok(Some(context)) = canvas.get_context("webgpu")
        && let Ok(method) =
            Reflect::get(&context, &"unconfigure".into()).and_then(JsCast::dyn_into::<Function>)
    {
        let _ = method.call0(&context);
    }
}

/// Terminal page-host teardown: close every mount, stop the loop, then call
/// `done`. A later mount starts a fresh loop.
///
/// # Errors
///
/// Rejects a second shutdown while one is pending.
#[wasm_bindgen]
pub fn shutdown(done: Function) -> Result<(), JsValue> {
    if STOP.with_borrow(Option::is_some) {
        return Err(error("shutdown already pending"));
    }
    let ids = MOUNTS.with_borrow(|m| m.keys().copied().collect::<Vec<_>>());
    for id in ids {
        close(id)?;
    }
    REQUESTS.with_borrow_mut(HashMap::clear);
    STOP.with_borrow_mut(|s| *s = Some(done));
    if let Some(p) = LOOP.with_borrow(Clone::clone) {
        p.send_event(0).map_err(error)?;
    } else {
        finish_shutdown();
    }
    Ok(())
}

fn finish_shutdown() {
    LOOP.with_borrow_mut(|p| *p = None);
    // Winit resets its creation guard after LoopExiting returns; the
    // microtask runs after that.
    if let Some(done) = STOP.with_borrow_mut(Option::take) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
                .await;
            let _ = done.call0(&JsValue::NULL);
        });
    }
}

struct Runner;

impl ApplicationHandler<u64> for Runner {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, id: u64) {
        if id == 0 && STOP.with_borrow(Option::is_some) {
            event_loop.exit();
            return;
        }
        if let Some(request) = REQUESTS.with_borrow_mut(|r| r.remove(&id)) {
            let status = request.status.clone();
            if let Err(e) = begin(event_loop, id, request) {
                emit(
                    &status,
                    &serde_json::json!({"phase":"failed","error":e.as_string().unwrap_or_else(||format!("{e:?}"))}),
                );
            }
            return;
        }
        MOUNTS.with_borrow_mut(|mounts| {
            if let Some(m) = mounts.get_mut(&id) {
                if let Some(failure) = m.gpu.as_ref().and_then(Gpu::failure) {
                    m.error = Some(failure);
                    m.publish(id);
                } else {
                    m.window.request_redraw();
                }
            }
        });
    }

    fn window_event(&mut self, _: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        MOUNTS.with_borrow_mut(|mounts| {
            let Some((&id, m)) = mounts.iter_mut().find(|(_, m)| m.window.id() == window_id) else {
                return;
            };
            match event {
                WindowEvent::RedrawRequested => {
                    if let Err(e) = m.draw() {
                        m.error = Some(e);
                        m.publish(id);
                    }
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if m.queue_resize() && !m.viewport.suspended() {
                        m.window.request_redraw();
                    }
                }
                WindowEvent::Occluded(hidden) => {
                    m.hidden = hidden;
                    if !hidden {
                        m.pending = true;
                        m.window.request_redraw();
                    }
                }
                WindowEvent::Destroyed => {}
                mut event => {
                    // The old layout stays on screen (stretched by CSS) until
                    // redraw commits the requested viewport; map pointer and
                    // touch coordinates back onto it.
                    match &mut event {
                        WindowEvent::CursorMoved { position, .. } => {
                            (position.x, position.y) =
                                m.viewport.map_pointer(position.x, position.y);
                        }
                        WindowEvent::Touch(touch) => {
                            (touch.location.x, touch.location.y) =
                                m.viewport.map_pointer(touch.location.x, touch.location.y);
                        }
                        _ => {}
                    }
                    let start = m.metric_start();
                    crate::clipboard::during_input(&m.status, || m.frame.send_host_event(&event));
                    m.metric_end("input", start);
                    m.pending = true;
                    m.window.request_redraw();
                }
            }
        });
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = ui::time::now();
        let mut deadline: Option<ui::time::Instant> = None;
        let mut wake =
            |at: ui::time::Instant| deadline = Some(deadline.map_or(at, |old| old.min(at)));
        MOUNTS.with_borrow(|mounts| {
            for (&id, m) in mounts {
                // A throttled semantics tree is delivered at its deadline even
                // when no further frame is presented.
                let semantics_due = if m.hidden || m.error.is_some() {
                    None
                } else {
                    m.semantics.borrow().due()
                };
                if let Some(due) = semantics_due {
                    if due <= now {
                        m.publish(id);
                    } else {
                        wake(due);
                    }
                }
                if m.hidden || m.error.is_some() || m.gpu.is_none() || m.viewport.suspended() {
                    continue;
                }
                let output = m.frame.frame_schedule();
                let desktop_woken = m
                    .desktop
                    .as_ref()
                    .is_some_and(|d| d.shared.woken.load(std::sync::atomic::Ordering::Acquire));
                if m.pending
                    || desktop_woken
                    || m.viewport.needs_present()
                    || output.queued_work
                    || output.needs_animation_frame
                {
                    m.window.request_redraw();
                }
                if let Some(at) = output.next_wake_deadline {
                    if at <= now {
                        m.window.request_redraw();
                    } else {
                        wake(at);
                    }
                }
            }
        });
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        finish_shutdown();
    }
}

/// The single application `app` names; `None` for the session desktop.
fn definition(
    app: Option<&str>,
) -> Result<Option<&'static ui::application::ApplicationDefinition>, JsValue> {
    match app {
        None | Some("desktop") => Ok(None),
        Some("scratch") => Ok(Some(crate::scratch::definition())),
        Some(app) => crate::desktop::application(app)
            .map(Some)
            .ok_or_else(|| error("unsupported application")),
    }
}

/// What a mount presents, before the window and frame pipeline exist.
struct Presentation {
    root: Root,
    stores: Rc<RefCell<Vec<RecordStore>>>,
    checkout: Option<RecordCheckout>,
    images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    desktop: Option<Desktop>,
}

enum Root {
    Component(Box<ui::application::AppComponent>),
    Desktop(Box<tilo_desktop_surface::DesktopSurface>),
}

fn present_desktop(
    id: u64,
    config: &Config,
    dyn_store: Rc<dyn durable_store::DurableStore>,
    permissions: Rc<RefCell<PermissionContext>>,
) -> Result<Presentation, JsValue> {
    let views = Rc::new(
        InstanceViewStore::with_store(Rc::clone(&dyn_store), &config.device, &config.room)
            .map_err(error)?,
    );
    let wake = proxy()?;
    let (surface, handle, shared, persistence) = crate::desktop::mount(
        crate::desktop::Inputs {
            owners: RecordOwners::with_store(Rc::clone(&dyn_store)),
            views,
            device: config.device.clone(),
            salt: config
                .peer
                .and_then(crate::replication::device_of)
                .unwrap_or_else(|| config.device.clone()),
            room: config.room.clone(),
            author: config.author(),
            permissions,
        },
        dyn_store,
        config.apps.as_deref(),
        Arc::new(move || {
            let _ = wake.send_event(id);
        }),
    )
    .map_err(error)?;
    Ok(Presentation {
        root: Root::Desktop(Box::new(surface)),
        stores: Rc::clone(&shared.stores),
        checkout: None,
        images: Rc::clone(&shared.images),
        desktop: Some(Desktop {
            handle,
            shared,
            persistence,
            generation: format!("mount-{id}"),
        }),
    })
}

fn present_component(
    definition: &'static ui::application::ApplicationDefinition,
    config: &Config,
    dyn_store: Rc<dyn durable_store::DurableStore>,
    permissions: Rc<RefCell<PermissionContext>>,
) -> Result<Presentation, JsValue> {
    let record = RecordOwners::with_store(Rc::clone(&dyn_store))
        .open(definition.app_id, &config.room)
        .map_err(error)?;
    // The instance's default record, named by the host so a reload opens the
    // same document, as a native launch does.
    let default = RecordCheckout::new(
        RecordId::derived("default", &RecordId::for_tab(config.instance)),
        definition.record_kind.clone(),
    );
    let (checkout, named_new) = match &config.record {
        Some(existing) => (
            Some(RecordCheckout::new(
                RecordId::parse(existing).ok_or_else(|| error("invalid record id"))?,
                definition.record_kind.clone(),
            )),
            None,
        ),
        None if config.new_document || definition.keeps_room_record() => (None, None),
        None => match record.try_open_record(&default) {
            Ok(_) => (Some(default), None),
            Err(ui::record::RecordOpenError::MissingRecord) => (None, Some(default.id)),
            Err(open) => return Err(error(open)),
        },
    };
    let target = match checkout {
        Some(checkout) => LaunchTarget::Record {
            checkout,
            reveal: None,
        },
        None if named_new.is_some() => LaunchTarget::New,
        None => definition.unsaved_target(&record),
    };
    let views = Rc::new(
        InstanceViewStore::with_store(dyn_store, &config.device, &config.room).map_err(error)?,
    );
    let restore = views
        .load(config.instance)
        .map_err(error)?
        .map_or(RestoreInput::None, RestoreInput::Local);
    let context = match named_new {
        Some(id) => HostContext::prepare_new_record(
            definition,
            HostContextInputs {
                records: record.clone(),
                views,
                device_id: config.device.clone(),
                instance_id: config.instance,
                permissions,
                author: UserId(config.author()),
                action_context: tilo_actions::ActionContext::default(),
            },
            id,
        ),
        None => HostContext::prepare(
            definition,
            HostContextInputs {
                records: record.clone(),
                views,
                device_id: config.device.clone(),
                instance_id: config.instance,
                permissions,
                author: UserId(config.author()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        ),
    }
    .map_err(error)?;
    let loaders = Rc::new(RefCell::new(Vec::new()));
    let destination = Rc::clone(&loaders);
    let component = ui::ImageStore::with_host_factory_with_max_dimension(
        Rc::new(move |max_dimension| {
            let (image, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
            destination.borrow_mut().push(loader);
            image
        }),
        || definition.instantiate(context, target, restore),
    )
    .map_err(error)?;
    let checkout = component.record().clone();
    Ok(Presentation {
        root: Root::Component(Box::new(component)),
        stores: Rc::new(RefCell::new(vec![record])),
        checkout: Some(checkout),
        images: loaders,
        desktop: None,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one ordered mount: storage, records, presentation, fonts, window, frame pipeline"
)]
fn begin(event_loop: &ActiveEventLoop, id: u64, request: Request) -> Result<(), JsValue> {
    let Request {
        canvas,
        config,
        fonts,
        hydrated,
        status,
    } = request;
    let definition = definition(config.app.as_deref())?;
    let store = crate::storage::load(&config.profile, &config.device, &hydrated)?;
    let dyn_store: Rc<dyn durable_store::DurableStore> = store.clone();
    let permissions = Rc::new(RefCell::new(PermissionContext::from_capabilities(
        collab_types::caps::Actor::person(Some(collab_types::role::Role::Owner), config.author()),
        Vec::new(),
    )));
    let Presentation {
        root,
        stores,
        checkout,
        images,
        desktop,
    } = match definition {
        Some(definition) => present_component(definition, &config, dyn_store, permissions)?,
        None => present_desktop(id, &config, dyn_store, permissions)?,
    };
    let system = ui::font_catalog::FontCatalog::product("", "en-US")
        .load_browser_startup_bytes(|name| {
            Reflect::get(&fonts, &name.into())
                .ok()
                .filter(|v| !v.is_undefined())
                .map(|v| Uint8Array::new(&v).to_vec())
        })
        .map_err(error)?;
    let window = Arc::new(
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_canvas(Some(canvas.clone()))
                    .with_append(false)
                    .with_prevent_default(true),
            )
            .map_err(error)?,
    );
    let size = window.inner_size();
    let observed = Viewport {
        width: size.width,
        height: size.height,
        dpr: window.scale_factor(),
    };
    let caps = if config.conservative_gpu {
        SurfaceCaps::CONSERVATIVE
    } else {
        SurfaceCaps::STANDARD
    };
    let viewport = observed.bounded_by(caps);
    let dpr = viewport.dpr;
    let text_renderer = ui::TextRenderer::with_browser_font_system(system);
    let mut frame = match root {
        Root::Component(component) => {
            let wake = proxy()?;
            FrameHarness::from_component_with_wake_notifier(
                *component,
                viewport.logical_size(),
                ComponentRunnerInputs {
                    theme: ui::Theme::dark(),
                    text_renderer,
                },
                move || {
                    let _ = wake.send_event(id);
                },
            )
            .map_err(error)?
        }
        Root::Desktop(surface) => {
            if let Some(desktop) = &desktop {
                desktop
                    .handle
                    .set_viewport_width(f64::from(viewport.logical_size().0));
            }
            FrameHarness::new_with_controlled_inputs(
                ui::state::State::new(),
                surface,
                viewport.logical_size(),
                ui::Theme::dark(),
                text_renderer,
                UserId(config.author()),
            )
        }
    };
    frame.set_input_scale(dpr).map_err(error)?;
    if desktop.is_some() {
        // The desktop's own search answers the command chord.
        frame.set_caller_palette_enabled(false);
    }
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(if config.mac {
        ui::platform::Platform::MacOs
    } else {
        ui::platform::Platform::Linux
    }));
    let replica_changed = Rc::new(std::cell::Cell::new(false));
    if let Some(peer) = config.peer {
        let wake = proxy()?;
        let attached = crate::replication::attach(
            peer,
            id,
            crate::replication::Attachment {
                stores: Rc::clone(&stores),
                desktop: desktop
                    .as_ref()
                    .map(|d| (d.handle.clone(), d.shared.clone())),
                changed: Rc::clone(&replica_changed),
                notify: Arc::new(move || {
                    let _ = wake.send_event(id);
                }),
            },
        );
        if !attached {
            return Err(error("the peer has no replicated room"));
        }
    }
    let window_ms = config
        .semantics_window_ms
        .unwrap_or(crate::semantics_status::SEMANTICS_WINDOW_MS);
    let mount = Mount {
        metrics: RefCell::default(),
        semantics: RefCell::new(crate::semantics_status::SemanticsGate::new(
            ui::time::Duration::from_millis(window_ms),
        )),
        window: Arc::clone(&window),
        canvas,
        frame,
        store,
        stores,
        checkout,
        desktop,
        gpu: None,
        pending: true,
        viewport: ViewportState::with_caps(observed, caps),
        caps,
        hidden: false,
        generation: 1,
        frames: 0,
        status: status.clone(),
        images,
        error: None,
        oversized_scene: false,
        peer: config.peer,
        replica_changed,
        focus_presence: RefCell::new(None),
    };
    MOUNTS.with_borrow_mut(|m| m.insert(id, mount));
    emit(&status, &serde_json::json!({"phase":"gpu-pending","id":id}));
    initialize_gpu(id, 1, window, caps);
    Ok(())
}

fn initialize_gpu(id: u64, generation: u64, window: Arc<Window>, caps: SurfaceCaps) {
    GPU_SETUPS.with_borrow_mut(|setups| {
        setups.push_back(GpuSetup {
            id,
            generation,
            window,
            caps,
        });
    });
    if GPU_SETUP_RUNNING.replace(true) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async {
        while let Some(setup) = GPU_SETUPS.with_borrow_mut(std::collections::VecDeque::pop_front) {
            set_up_gpu(setup).await;
        }
        GPU_SETUP_RUNNING.set(false);
    });
}

/// The shared device and renderer, created with the first surface.
async fn shared_gpu(
    window: &Arc<Window>,
    size: Viewport,
    caps: SurfaceCaps,
) -> Result<(Rc<RefCell<SharedGpu>>, vello::util::RenderSurface<'static>), String> {
    let live = SHARED_GPU
        .with_borrow(Clone::clone)
        .filter(|s| s.try_borrow().is_ok_and(|s| s.failure().is_none()));
    if let Some(shared) = live {
        let surface = {
            // Draws of other mounts skip a frame while this is held.
            let mut guard = shared.try_borrow_mut().map_err(|_| "shared GPU busy")?;
            guard
                .cx
                .create_surface(
                    Arc::clone(window),
                    size.width.max(1),
                    size.height.max(1),
                    wgpu::PresentMode::Fifo,
                )
                .await
                .map_err(|e| e.to_string())?
        };
        return Ok((shared, surface));
    }
    let mut cx = ui::gpu::RenderContext::new();
    let surface = cx
        .create_surface(
            Arc::clone(window),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::Fifo,
        )
        .await
        .map_err(|e| e.to_string())?;
    let handle = cx
        .devices
        .get(surface.dev_id)
        .ok_or("WebGPU device missing")?;
    let mut renderer = vello::Renderer::new(
        &handle.device,
        vello::RendererOptions {
            antialiasing_support: vello::AaSupport::area_only(),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    if caps == SurfaceCaps::CONSERVATIVE {
        renderer.set_bump_budget_limit(CONSERVATIVE_BUMP_BUDGET_BYTES);
        renderer.set_bump_budget_grow_once(true);
        renderer.set_image_atlas_max(CONSERVATIVE_IMAGE_ATLAS_MAX);
    } else {
        renderer.set_bump_budget_limit(MAX_BUMP_BUDGET_BYTES);
    }
    let p = proxy().map_err(|e| format!("{e:?}"))?;
    let failure = Arc::new(Mutex::new(None));
    let mounts: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let wake = move |p: &EventLoopProxy<u64>, mounts: &Mutex<Vec<u64>>| {
        for &id in mounts.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            let _ = p.send_event(id);
        }
    };
    let (lost, lost_mounts, lost_proxy) = (Arc::clone(&failure), Arc::clone(&mounts), p.clone());
    handle
        .device
        .set_device_lost_callback(move |reason, message| {
            *lost.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!(
                "WebGPU device lost: {reason:?}: {message}; call recover"
            ));
            wake(&lost_proxy, &lost_mounts);
        });
    let (uncaptured, error_mounts) = (Arc::clone(&failure), Arc::clone(&mounts));
    handle.device.on_uncaptured_error(Arc::new(move |e| {
        *uncaptured.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(format!("WebGPU error: {e}; call recover"));
        wake(&p, &error_mounts);
    }));
    let shared = Rc::new(RefCell::new(SharedGpu {
        cx,
        renderer,
        failure,
        mounts,
    }));
    SHARED_GPU.with_borrow_mut(|s| *s = Some(Rc::clone(&shared)));
    Ok((shared, surface))
}

async fn set_up_gpu(setup: GpuSetup) {
    let GpuSetup {
        id,
        generation,
        window,
        caps,
    } = setup;
    let current =
        MOUNTS.with_borrow(|mounts| mounts.get(&id).is_some_and(|m| m.generation == generation));
    if !current {
        return;
    }
    let size = window.inner_size();
    let size = Viewport {
        width: size.width,
        height: size.height,
        dpr: window.scale_factor(),
    }
    .bounded_by(caps);
    let result = shared_gpu(&window, size, caps).await;
    let installed = MOUNTS.with_borrow_mut(|mounts| {
        let Some(m) = mounts.get_mut(&id).filter(|m| m.generation == generation) else {
            return false;
        };
        match result {
            Ok((shared, surface)) => {
                if let Ok(s) = shared.try_borrow() {
                    s.mounts
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(id);
                }
                emit(
                    &m.status,
                    &serde_json::json!({"initialSurfaceSize":[surface.config.width,surface.config.height]}),
                );
                m.gpu = Some(Gpu { shared, surface });
                m.viewport.surface_installed();
                m.queue_resize();
                m.pending = true;
                m.window.request_redraw();
                true
            }
            Err(e) => {
                m.error = Some(format!("WebGPU unavailable: {e}"));
                m.publish(id);
                false
            }
        }
    });
    // GPU setup completes outside winit dispatch. Wake an idle loop after
    // releasing the mount borrow so the new generation actually presents.
    if installed && let Ok(wake) = proxy() {
        let _ = wake.send_event(id);
    }
}

impl Mount {
    fn metric_start(&self) -> Option<web_time::Instant> {
        self.metrics.borrow().begin()
    }

    fn metric_end(&self, stage: &'static str, start: Option<web_time::Instant>) {
        self.metrics.borrow_mut().end(stage, start);
    }

    // ResizeObserver/winit is the sole source of viewport notifications.
    // Reading the pair again at redraw also covers scale/size event ordering
    // and changes that occurred while GPU initialization was pending.
    fn queue_resize(&mut self) -> bool {
        let size = self.window.inner_size();
        let changed = self.viewport.request(Viewport {
            width: size.width,
            height: size.height,
            dpr: self.window.scale_factor(),
        });
        self.pending |= changed;
        changed
    }

    fn draw(&mut self) -> Result<(), String> {
        let start = self.metric_start();
        let before = self.frames;
        let result = self.draw_measured();
        self.metric_end(
            if self.frames > before {
                "draw_total"
            } else {
                "draw_skipped"
            },
            start,
        );
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "configure, acquire, render, submit and present stay in one synchronous redraw"
    )]
    fn draw_measured(&mut self) -> Result<(), String> {
        if self.hidden || self.error.is_some() {
            return Ok(());
        }
        self.queue_resize();
        if self.viewport.suspended() {
            return Ok(());
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let viewport = self.viewport.requested();
        // What asked for this frame, when frame metrics are on.
        let mut reasons = self.metrics.borrow().enabled().then(|| {
            let mut reasons = Vec::new();
            if self.pending {
                reasons.push("input or host".to_owned());
            }
            reasons
        });
        // Desktop tabs' components and queued desktop changes wake the
        // mount, not the frame harness. Relayout to take them, and present
        // the new scene.
        if let Some(desktop) = self.desktop.as_ref().filter(|d| d.shared.take_wake()) {
            let wakers = desktop.shared.take_wakers();
            if let Some(reasons) = &mut reasons {
                if wakers.is_empty() {
                    reasons.push("desktop wake".into());
                }
                reasons.extend(wakers.iter().map(|app| format!("wake: {app}")));
            }
            self.frame.relayout();
            self.pending = true;
        }
        // A peer's change merged into an open record.
        if self.replica_changed.replace(false) {
            if let Some(reasons) = &mut reasons {
                reasons.push("replica change".into());
            }
            self.frame.relayout();
            self.pending = true;
        }
        let frame = self.frame.frame_schedule();
        let due = frame
            .next_wake_deadline
            .is_some_and(|wake| wake <= ui::time::now());
        if let Some(reasons) = &mut reasons {
            if self.viewport.needs_present() {
                reasons.push("viewport".into());
            }
            if frame.queued_work {
                reasons.push("queued work".into());
            }
            if frame.needs_animation_frame || due {
                reasons.extend(
                    self.frame
                        .frame_requesters()
                        .into_iter()
                        .map(|(path, demand)| match demand {
                            ui::headless::FrameDemand::Animation => format!("animation: {path}"),
                            ui::headless::FrameDemand::Deadline => format!("deadline: {path}"),
                        }),
                );
            }
        }
        self.pending |= self.viewport.needs_present()
            || frame.queued_work
            || frame.needs_animation_frame
            || due;
        if !self.pending {
            return Ok(());
        }
        let Ok(mut shared_guard) = gpu.shared.try_borrow_mut() else {
            // Another mount's GPU setup holds the shared device for a moment.
            self.window.request_redraw();
            return Ok(());
        };
        let shared = &mut *shared_guard;
        let handle = shared
            .cx
            .devices
            .get(gpu.surface.dev_id)
            .ok_or("WebGPU device missing")?;
        if let Some(reason) = handle.loss_monitor().reason() {
            return Err(format!(
                "WebGPU device lost: {reason}; call recover to recreate the renderer"
            ));
        }
        // Configuration changes canvas.width/height on WebGPU, clearing its
        // displayed image. Keep configure -> acquire -> submit -> present in
        // this synchronous redraw callback, with no equal-size resize.
        if self
            .viewport
            .needs_surface_resize(gpu.surface.config.width, gpu.surface.config.height)
        {
            shared
                .cx
                .resize_surface(&mut gpu.surface, viewport.width, viewport.height);
        }
        let mut acquired = gpu.surface.surface.get_current_texture();
        if matches!(acquired, wgpu::CurrentSurfaceTexture::Outdated)
            && self.viewport.recover_outdated()
        {
            shared.cx.configure_surface(&gpu.surface);
            acquired = gpu.surface.surface.get_current_texture();
        }
        let output = match acquired {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded => None,
            other => return Err(format!("WebGPU surface unavailable: {other:?}")),
        };
        drop(shared_guard);
        let frame = &mut self.frame;
        let Some(output) = self
            .viewport
            .apply_for_acquired_texture(output, |viewport| {
                frame.set_input_scale(viewport.dpr)?;
                frame
                    .resize(viewport.logical_size())
                    .map_err(|e| format!("{e:?}"))
            })?
        else {
            // Keep the old geometry and pending work on a skipped acquisition;
            // winit coalesces the retry into the next animation frame.
            self.window.request_redraw();
            return Ok(());
        };
        if let Some(desktop) = &self.desktop {
            desktop
                .handle
                .set_viewport_width(f64::from(viewport.logical_size().0));
        }
        // Poll only after applying the new viewport, avoiding an old-size
        // layout merely to discover work.
        self.frame.poll_frame().map_err(|e| format!("{e:?}"))?;
        self.save_desktop();
        if let Some(peer) = self.peer {
            crate::replication::publish(peer);
        }
        let frame = self.frame.frame_render_input();
        // Never hand the renderer a scene it cannot resolve: an oversized
        // encoding overflows vello's resolver or WebGPU buffer limits. Keep the
        // last presented frame, report once, and keep the session alive.
        if let Err(reason) = scene_within_limits(frame.scene) {
            if !self.oversized_scene {
                self.oversized_scene = true;
                web_sys::console::error_1(&JsValue::from_str(&reason));
            }
            self.pending = false;
            return Ok(());
        }
        self.oversized_scene = false;
        let mut physical = vello::Scene::new();
        physical.append(
            frame.scene,
            Some(ui::scale::composite_transform(viewport.dpr)),
        );
        let gpu = self.gpu.as_mut().ok_or("WebGPU renderer missing")?;
        let mut shared_guard = gpu.shared.try_borrow_mut().map_err(|_| "shared GPU busy")?;
        let shared = &mut *shared_guard;
        let handle = shared
            .cx
            .devices
            .get(gpu.surface.dev_id)
            .ok_or("WebGPU device missing")?;
        shared
            .renderer
            .render_to_texture(
                &handle.device,
                &handle.queue,
                &physical,
                &gpu.surface.target_view,
                &vello::RenderParams {
                    base_color: frame.background,
                    width: viewport.width,
                    height: viewport.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| e.to_string())?;
        let mut encoder = handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        gpu.surface.blitter.copy(
            &handle.device,
            &mut encoder,
            &gpu.surface.target_view,
            &output
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        handle.queue.submit([encoder.finish()]);
        drop(shared_guard);
        output.present();
        self.viewport.did_present();
        self.pending = false;
        self.frames = self.frames.saturating_add(1);
        if let Some(reasons) = reasons {
            self.metrics.borrow_mut().presented(reasons);
        }
        self.publish(0);
        Ok(())
    }

    /// The published caret or selection as live selection anchors
    /// (`{anchor, head}`, hex), for the page's live layer: the keyboard
    /// focus's when it is in the focused window (or this mount's
    /// application), else that window's editor caret
    /// ([`tilo_desktop_surface::published_selection`]).
    fn focus_presence_value(&self) -> Option<serde_json::Value> {
        use ui::collaboration::PresencePayload;
        let keyboard = self.frame.focus_presence();
        let (key, payload) = match &self.desktop {
            Some(desktop) => desktop.handle.published_selection(keyboard),
            None => tilo_desktop_surface::published_selection(
                keyboard,
                &ui::presence::capability::subtree_presence(self.frame.root_widget()),
            ),
        }?;
        let (start, end) = match payload {
            PresencePayload::Caret { pos } => (pos, pos),
            PresencePayload::Selection { start, end } => (start, end),
            _ => return None,
        };
        let (anchor, head) = tilo_browser_peer::transport::presence_anchors(&key, start, end)?;
        Some(
            serde_json::json!({"anchor": hex::encode(anchor.cursor), "head": hex::encode(head.cursor)}),
        )
    }

    fn publish(&self, id: u64) {
        let ime = self.frame.ime_window().map(|i| {
            serde_json::json!({"x":i.x,"y":i.y,"width":i.width,"height":i.height,"purpose":format!("{:?}",i.purpose)})
        });
        let semantics = self.frame.inspection();
        // The semantics tree is the bulk of every status; the host keeps the
        // last tree it received, so an unchanged tree is left out and
        // content-only changes while typing are throttled.
        let decision = self.semantics.borrow_mut().decide(
            &semantics,
            self.frame.ime_window().as_ref(),
            ui::time::now(),
        );
        let applied = self.viewport.applied();
        let value = serde_json::json!({
            "phase": if self.error.is_some() { "failed" } else { "presented" },
            "id": id,
            "error": self.error,
            "frames": self.frames,
            "gpuGeneration": self.generation,
            "logicalSize": applied.logical_size(),
            "dpr": applied.dpr,
            "surfaceSize": self.gpu.as_ref().map(|g| [g.surface.config.width, g.surface.config.height]),
            "rootRect": semantics.root.rect,
            "record": self.checkout.as_ref().map(|checkout| checkout.id.as_str()),
            "desktopRevision": self.desktop.as_ref().map(|d| d.handle.revision()),
            "searchOpen": self.desktop.as_ref().is_some_and(|d| d.handle.search_open()),
            "ime": ime,
        });
        let mut value = value;
        let presence = self.focus_presence_value();
        if *self.focus_presence.borrow() != presence
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "focusPresence".into(),
                presence.clone().unwrap_or(serde_json::Value::Null),
            );
            *self.focus_presence.borrow_mut() = presence;
        }
        let mut text = value.to_string();
        if let crate::semantics_status::SemanticsDecision::Deliver { .. } = decision {
            // Splice the already serialized tree into the status object
            // instead of parsing it back into a value and serializing twice.
            let gate = self.semantics.borrow();
            text.pop();
            text.push_str(",\"semantics\":");
            text.push_str(std::str::from_utf8(gate.text()).unwrap_or("null"));
            text.push('}');
        }
        emit_text(&self.status, &text);
    }
}

/// Deliver an IME event (`start`, `preedit`, `commit`, `end`) from the
/// page's hidden text editor.
///
/// # Errors
///
/// Rejects an unknown kind or a closed mount.
#[wasm_bindgen]
pub fn ime(id: u64, kind: &str, text: &str) -> Result<(), JsValue> {
    let event = match kind {
        "start" => winit::event::Ime::Enabled,
        "preedit" => winit::event::Ime::Preedit(text.into(), Some((text.len(), text.len()))),
        "commit" => winit::event::Ime::Commit(text.into()),
        "end" => winit::event::Ime::Disabled,
        _ => return Err(error("invalid IME event")),
    };
    with_mount(id, |m| {
        m.frame.send_host_event(&WindowEvent::Ime(event));
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Perform an accessibility action from the semantics DOM projection.
///
/// # Errors
///
/// Rejects an unsupported action, a closed mount, or a refused action.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn semantic(
    id: u64,
    path: &str,
    target: Option<String>,
    action: &str,
    value: &str,
    anchor: usize,
    focus: usize,
) -> Result<bool, JsValue> {
    use ui::a11y::Action;
    let action = match action {
        "focus" => Action::Focus,
        "activate" => Action::Activate,
        "toggle" => Action::ToggleCheck,
        "value" => Action::SetValue(value.into()),
        "selection" => Action::SetSelection { anchor, focus },
        _ => return Err(error("unsupported semantic action")),
    };
    with_mount(id, |m| {
        crate::clipboard::during_input(&m.status, || {
            m.frame
                .perform_semantic_named(path, target.as_deref(), &action)
        })
        .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(true)
    })
}

/// The actions the mount's application publishes, described as
/// `tilo-control actions list` describes them. `resource` narrows to one
/// resource and its children, including one the app resolves lazily. On a
/// desktop, the actions of one window's application: `window` (its tab id),
/// else the focused window's.
///
/// # Errors
///
/// Rejects a closed mount, an unknown or absent window, or an unavailable
/// application.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn component_actions(
    id: u64,
    resource: Option<String>,
    window: Option<String>,
) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let instance = m.action_window(window.as_deref())?;
        let resources = m
            .frame
            .bound_resources_of(instance, resource.as_deref())
            .map_err(|e| door_error(&e))?;
        serde_json::to_string(&tilo_actions::door::survey(&resources)).map_err(error)
    })
}

/// Invoke one action the mount's application publishes, through the bound
/// door the palette, agents and assistive technology use: the address must
/// be published now, the mount's permissions must allow it, its arguments
/// must bind, and a confirmation policy requires `confirmed`. `window` is
/// as in [`component_actions`].
///
/// # Errors
///
/// Rejects a closed mount, malformed arguments, or a refused action (the
/// message starts with the door's error code).
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn invoke_component_action(
    id: u64,
    resource: &str,
    action: &str,
    arguments: &str,
    confirmed: bool,
    window: Option<String>,
) -> Result<String, JsValue> {
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(arguments).map_err(error)?;
    with_mount(id, |m| {
        let instance = m.action_window(window.as_deref())?;
        let address = ui::bound_dispatch::BoundActionAddress {
            instance_id: instance
                .or_else(|| m.frame.component_instance_id())
                .ok_or_else(|| error("the mount hosts no application"))?,
            address: tilo_actions::ActionAddress::new(resource, action),
        };
        let started = if confirmed {
            m.frame.invoke_bound_action_confirmed(&address, &arguments)
        } else {
            m.frame.invoke_bound_action(&address, &arguments)
        }
        .map_err(|e| door_error(&e))?;
        // A following call sees the state this one left, not the last frame's.
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(match started {
            ui::widget::ResourceOperationStart::Completed(output) => {
                serde_json::json!({"state": "completed", "output": output})
            }
            ui::widget::ResourceOperationStart::Accepted => {
                serde_json::json!({"state": "accepted"})
            }
        }
        .to_string())
    })
}

fn door_error(e: &tilo_actions::DoorError) -> JsValue {
    error(format!("{}: {e}", e.code()))
}

impl Mount {
    /// The application instance a page's action call addresses: `None` for
    /// a single-application mount, a desktop's `window` (tab id) or else its
    /// focused window.
    fn action_window(&self, window: Option<&str>) -> Result<Option<u64>, JsValue> {
        let Some(desktop) = &self.desktop else {
            return match window {
                None => Ok(None),
                Some(_) => Err(error("a single-application mount has no windows")),
            };
        };
        let tab = match window {
            Some(window) => window
                .parse::<u64>()
                .ok()
                .filter(|tab| tab.to_string() == window)
                .ok_or_else(|| error("invalid window identity"))?,
            None => desktop
                .handle
                .read(tilo_desktop_surface::navigation::active_route)
                .map(|route| route.tab)
                .ok_or_else(|| error("no window is focused"))?,
        };
        Ok(Some(tab))
    }
}

/// The next status carries the complete semantics tree, for example when
/// assistive technology focus enters the surface.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn request_semantics(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.semantics.borrow_mut().request();
        m.publish(id);
        Ok(())
    })
}

/// The desktop's `DesktopNavigationV1` snapshot as JSON; `None` for a
/// single-application mount.
///
/// # Errors
///
/// Rejects a closed mount or a failure to serialize the snapshot.
#[wasm_bindgen]
pub fn desktop_navigation_snapshot(id: u64) -> Result<Option<String>, JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                serde_json::to_string(
                    &d.handle
                        .snapshot(&d.generation, crate::desktop::catalogue()),
                )
                .map_err(error)
            })
            .transpose()
    })
}

/// Where the desktop shows each record right now: `[{record, rect: [x0, y0,
/// x1, y1]}]` for the active tab of every window in the shown strip, in the
/// surface's CSS pixels. The page draws a collaborator's pointer inside the
/// window of the record it is focused on. `None` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_record_windows(id: u64) -> Result<Option<String>, JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let windows: Vec<serde_json::Value> = d
                    .handle
                    .shown_tabs()
                    .into_iter()
                    .filter_map(|(tab, rect)| {
                        let record = d.shared.record_of(tab)?;
                        Some(serde_json::json!({
                            "record": record,
                            "rect": [rect.x0, rect.y0, rect.x1, rect.y1],
                        }))
                    })
                    .collect();
                serde_json::to_string(&windows).map_err(error)
            })
            .transpose()
    })
}

/// Where the desktop shows each member's legacy window right now: `[{key,
/// rect: [x0, y0, x1, y1]}]` for windows whose active tab shows one in the
/// shown strip, in the surface's CSS pixels; empty while search covers the
/// desktop. `None` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_peer_windows(id: u64) -> Result<Option<String>, JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let windows: Vec<serde_json::Value> = if d.handle.search_open() {
                    Vec::new()
                } else {
                    d.handle
                        .shown_tabs()
                        .into_iter()
                        .filter_map(|(tab, rect)| {
                            Some(serde_json::json!({
                                "key": d.shared.peer_window_of(tab)?,
                                "rect": [rect.x0, rect.y0, rect.x1, rect.y1],
                            }))
                        })
                        .collect()
                };
                serde_json::to_string(&windows).map_err(error)
            })
            .transpose()
    })
}

/// Show exactly `windows` (`[{key, app, title}]` JSON), the legacy windows
/// other members stream, as desktop windows: a new key opens a window
/// beside the focused one, a key no longer listed closes its window. The
/// page draws each stream over its window's content
/// ([`desktop_peer_windows`]).
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_peer_windows(id: u64, windows: &str) -> Result<(), JsValue> {
    let wanted: Vec<crate::desktop::PeerWindow> = serde_json::from_str(windows).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let (launch, close) = desktop
            .handle
            .read(|tree| desktop.shared.reconcile_peer_windows(tree, wanted));
        if launch.is_empty() && close.is_empty() {
            return Ok(());
        }
        for tab in close {
            let route = desktop.handle.read(|tree| {
                tree.workspaces.iter().find_map(|workspace| {
                    workspace.strips.iter().find_map(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .find(|group| group.tabs.iter().any(|t| t.id == tab))
                            .map(|group| tilo_desktop_surface::navigation::Route {
                                workspace: workspace.id.0.to_string(),
                                strip: strip.id.0.to_string(),
                                group: group.id.to_string(),
                                tab: tab.to_string(),
                            })
                    })
                })
            });
            if let Some(route) = route {
                let command = tilo_desktop_surface::navigation::Command::Close {
                    generation: desktop.generation.clone(),
                    route,
                };
                // A window the person already closed is gone either way.
                let _ = desktop.handle.dispatch(&command, &desktop.generation);
            }
            desktop.shared.closed_peer_window(tab);
        }
        for app in launch {
            let command = tilo_desktop_surface::navigation::Command::Launch {
                generation: desktop.generation.clone(),
                app,
            };
            desktop
                .handle
                .dispatch(&command, &desktop.generation)
                .map_err(error)?;
        }
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Changes whenever the desktop tree or what the desktop presents beside it
/// (pan, scroll, history, activity) does, so the page republishes
/// navigation only when something moved. `0` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_navigation_revision(id: u64) -> Result<f64, JsValue> {
    with_mount(id, |m| {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a revision counter stays far below 2^53 and is only compared for change"
        )]
        Ok(m.desktop
            .as_ref()
            .map_or(0.0, |d| d.handle.navigation_revision() as f64))
    })
}

/// Dispatch a `DesktopNavigationCommand` (JSON). Acceptance queues the
/// change for the next frame; the page observes it in later snapshots.
///
/// # Errors
///
/// Rejects a closed or single-application mount, a malformed command, a
/// stale generation, an absent target, or a command the desktop does not
/// perform.
#[wasm_bindgen]
pub fn desktop_navigate(id: u64, command: &str) -> Result<(), JsValue> {
    let command: tilo_desktop_surface::navigation::Command =
        serde_json::from_str(command).map_err(error)?;
    if let tilo_desktop_surface::navigation::Command::Launch { app, .. } = &command
        && crate::desktop::application(app).is_none()
    {
        return Err(error(format!("{app} is not available in the browser")));
    }
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop navigation unavailable"))?;
        desktop
            .handle
            .dispatch(&command, &desktop.generation)
            .map_err(error)?;
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Open desktop search, or close it when open: the page's search shortcut
/// and search control. The frame that follows shows the palette.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_search_toggle(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop search unavailable"))?;
        desktop.handle.toggle_search();
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Replace the desktop's tree with one changed elsewhere (`DesktopTree`
/// JSON, for example from a peer). Tabs still present keep their running
/// applications; the change is saved to the profile like a local one.
///
/// # Errors
///
/// Rejects a closed or single-application mount or a malformed tree.
#[wasm_bindgen]
pub fn desktop_apply_tree(id: u64, tree: &str) -> Result<(), JsValue> {
    let tree = crate::desktop::parse_tree(tree).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.handle.replace_tree(tree);
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// The desktop's current tree as `DesktopTree` JSON, for peers; `None` for
/// a single-application mount.
///
/// # Errors
///
/// Rejects a closed mount or a failure to serialize the tree.
#[wasm_bindgen]
pub fn desktop_tree(id: u64) -> Result<Option<String>, JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| d.handle.read(serde_json::to_string).map_err(error))
            .transpose()
    })
}

/// A room member's presence from the page's live layer: `publisher` (the
/// member's device, hex) with its participant `label`, `kind` (`person` or
/// `agent`) and `color` (`#rrggbb`), and its caret or selection as the live
/// anchors (hex) it published, or none when it has no selection. Editors
/// draw the caret, labelled, in the next frame.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives optional JavaScript strings only as owned values"
)]
pub fn remote_presence(
    id: u64,
    publisher: &str,
    label: &str,
    kind: &str,
    color: &str,
    anchor: Option<String>,
    head: Option<String>,
) -> Result<(), JsValue> {
    use ui::collaboration::PresencePayload;
    let range = anchor.zip(head).and_then(|(anchor, head)| {
        let anchor = tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(anchor).ok()?,
        };
        let head = tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(head).ok()?,
        };
        tilo_browser_peer::transport::read_presence_anchors(&anchor, &head)
    });
    with_mount(id, |m| {
        let user = UserId(publisher.to_owned());
        let presence = m.frame.presence_mut();
        presence.set_participant_kind(&user, kind == "agent");
        if !label.is_empty() {
            presence.set_display_name(user.clone(), label);
        }
        presence.set_color_override(user, color);
        match range {
            Some((key, start, end)) => {
                let payload = if start == end {
                    PresencePayload::Caret { pos: start }
                } else {
                    PresencePayload::Selection { start, end }
                };
                presence.apply_remote_presence(publisher.to_owned(), key, payload);
            }
            None => presence.unregister_peer(publisher),
        }
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// A member left the room: forget its carets and name.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn remote_presence_gone(id: u64, publisher: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.frame.presence_mut().unregister_peer(publisher);
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Take accepted writes for the page to commit (`[{key, bytes, actor,
/// revision}]`).
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn pending_writes(id: u64) -> Result<JsValue, JsValue> {
    with_mount(id, |m| {
        m.flush_records();
        crate::storage::drain(&m.store)
    })
}

/// Recreate the surface, renderer and device without recreating documents.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn recover(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        unconfigure(&m.canvas);
        if let Some(gpu) = m.gpu.take() {
            release_gpu(id, gpu);
        }
        m.generation = m.generation.saturating_add(1);
        m.error = None;
        initialize_gpu(id, m.generation, Arc::clone(&m.window), m.caps);
        Ok(())
    })
}

/// Run a copy, cut or paste inside the trusted DOM clipboard event.
///
/// # Errors
///
/// Rejects an unknown kind or a closed mount.
#[wasm_bindgen]
pub fn clipboard_event(
    id: u64,
    kind: &str,
    transfer: JsValue,
    shift: bool,
) -> Result<bool, JsValue> {
    let key = match kind {
        "paste" => "v",
        "copy" => "c",
        "cut" => "x",
        _ => return Err(error("invalid clipboard event")),
    };
    crate::clipboard::during(transfer, || {
        with_mount(id, |m| {
            let mut modifiers =
                if m.frame.platform_conventions().platform() == ui::platform::Platform::MacOs {
                    winit::keyboard::ModifiersState::SUPER
                } else {
                    winit::keyboard::ModifiersState::CONTROL
                };
            if shift {
                modifiers |= winit::keyboard::ModifiersState::SHIFT;
            }
            m.frame.send_host_event(&WindowEvent::ModifiersChanged(
                winit::event::Modifiers::from(modifiers),
            ));
            // A failed clipboard sink must never let a cut delete the selection.
            if kind == "cut" {
                m.frame.key("c");
            }
            if kind != "cut" || crate::clipboard::copied() {
                m.frame.key(key);
            }
            m.frame.send_host_event(&WindowEvent::ModifiersChanged(
                winit::event::Modifiers::default(),
            ));
            m.pending = true;
            m.window.request_redraw();
            Ok(kind == "paste" || crate::clipboard::copied())
        })
    })
}

/// Optional canonical fonts the last shaping pass needed, as a JSON array.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn font_demands(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        serde_json::to_string(&m.frame.take_browser_font_demands()).map_err(error)
    })
}

/// Install one fetched optional font; the runtime verifies it against the
/// canonical manifest.
///
/// # Errors
///
/// Rejects a closed mount or bytes that fail verification.
#[wasm_bindgen]
pub fn install_font(id: u64, name: &str, bytes: Vec<u8>) -> Result<bool, JsValue> {
    let (changed, wake) = with_mount(id, |m| {
        let changed = m.frame.install_browser_font(name, bytes).map_err(error)?;
        let wake = changed && !m.hidden;
        if wake {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok((changed, wake))
    })?;
    if wake {
        proxy()?.send_event(id).map_err(error)?;
    }
    Ok(changed)
}

/// A font fetch failed; demand is re-reported by a later frame.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn font_failed(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.frame.browser_font_failed();
        Ok(())
    })
}

/// Image URLs the application asked for, as `[{loader, url}]`.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn asset_requests(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let requests = m
            .images
            .borrow_mut()
            .iter_mut()
            .enumerate()
            .flat_map(|(loader, images)| {
                images
                    .take_requests()
                    .into_iter()
                    .map(move |url| serde_json::json!({"loader":loader,"url":url}))
            })
            .collect::<Vec<_>>();
        serde_json::to_string(&requests).map_err(error)
    })
}

/// Deliver fetched image bytes (or `None` for a failed fetch).
///
/// # Errors
///
/// Rejects a closed mount or an unknown loader.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives JavaScript bytes only as an owned value"
)]
pub fn complete_asset(
    id: u64,
    loader: usize,
    url: &str,
    bytes: Option<Vec<u8>>,
) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let complete = m
            .images
            .borrow_mut()
            .get_mut(loader)
            .ok_or_else(|| error("unknown image loader"))?
            .complete(url, bytes.as_deref());
        // A decoded image changes what widgets measure and draw.
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(complete)
    })
}

/// Publish the DOM event's modifier snapshot before winit dispatches its key.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "mirrors KeyboardEvent's four modifier flags"
)]
pub fn modifiers(
    id: u64,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<(), JsValue> {
    use winit::keyboard::ModifiersState as M;
    let mut flags = M::empty();
    flags.set(M::CONTROL, control);
    flags.set(M::ALT, alt);
    flags.set(M::SHIFT, shift);
    flags.set(M::SUPER, meta);
    with_mount(id, |m| {
        m.frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(flags),
        ));
        Ok(())
    })
}

/// Follow `document.visibilityState`.
#[wasm_bindgen]
pub fn visible(id: u64, visible: bool) {
    MOUNTS.with_borrow_mut(|mounts| {
        if let Some(m) = mounts.get_mut(&id) {
            m.hidden = !visible;
            if visible {
                m.pending = true;
                m.window.request_redraw();
            }
        }
    });
}

/// Names applications requested through `ui::host_data` since the last
/// call, as a JSON array, for the page to fetch from the bundle's `data/`.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn data_requests() -> Result<String, JsValue> {
    serde_json::to_string(&ui::host_data::take_requests()).map_err(error)
}

/// Supply requested data, verified by the page against the bundle's
/// manifest, and relayout every mount so the requester sees it.
#[wasm_bindgen]
pub fn install_data(name: &str, bytes: Vec<u8>) {
    ui::host_data::install(name, bytes);
    relayout_all();
}

/// Report that requested data cannot be fetched.
#[wasm_bindgen]
pub fn data_failed(name: &str, reason: &str) {
    ui::host_data::fail(name, reason);
    relayout_all();
}

fn relayout_all() {
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut() {
            mount.frame.relayout();
            mount.pending = true;
            mount.window.request_redraw();
        }
    });
}

/// Opt-in bounded diagnostic capture; disabled by default and never persisted.
#[wasm_bindgen]
pub fn configure_frame_metrics(enabled: bool) {
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut() {
            mount.metrics.borrow_mut().configure(enabled);
            if enabled {
                mount.frame.begin_profiling();
            }
        }
    });
}

/// Drain captured stage timings and per-pass layout profiles, per mount.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn take_frame_metrics() -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let samples: Vec<_> = mounts
            .iter_mut()
            .map(|(id, mount)| {
                let mut metrics = mount.metrics.borrow_mut();
                serde_json::json!({"mount": id, "samples": metrics.take(), "dropped": metrics.take_dropped(), "requesters": metrics.take_requesters(), "layoutPasses": mount.frame.take_profile_samples()})
            })
            .collect();
        serde_json::to_string(&samples).map_err(error)
    })
}
