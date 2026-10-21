//! One page-lifetime winit loop. JavaScript owns the DOM and `IndexedDB`; this
//! owns the real application component, its frame pipeline and the GPU.
//!
//! Records and views live in `durable_store::MemoryStore` via
//! [`crate::storage`]. A mount is the session desktop ([`crate::desktop`])
//! unless the page names one application.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

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

use tilo_desktop::feedback::PrivateFeedback;

use crate::viewport::{SurfaceCaps, Viewport, ViewportState};
use tilo_lock::MutexRecover as _;

thread_local! {
    static FEEDBACK_STARTUPS: RefCell<HashMap<u32, tilo_desktop::feedback::transport::Startup>> =
        RefCell::new(HashMap::new());
    static LOOP: RefCell<Option<EventLoopProxy<u64>>> = const { RefCell::new(None) };
    static MOUNTS: RefCell<HashMap<u64, Mount>> = RefCell::new(HashMap::new());
    static SERVICING: RefCell<std::collections::HashSet<u64>> = RefCell::new(std::collections::HashSet::new());
    static REPLAYING: Cell<bool> = const { Cell::new(false) };
    static DEFERRED_EVENTS: RefCell<Vec<DeferredEvent>> = const { RefCell::new(Vec::new()) };
    static REQUESTS: RefCell<HashMap<u64, Request>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u64> = const { RefCell::new(1) };
    static STOP: RefCell<Option<Function>> = const { RefCell::new(None) };
    static SHARED_GPU: RefCell<Option<Rc<RefCell<SharedGpu>>>> = const { RefCell::new(None) };
    /// Mount GPU setups, run one at a time: opening a workspace while another
    /// sets up must not stack a second device creation on the first.
    static GPU_SETUPS: RefCell<std::collections::VecDeque<GpuSetup>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
    static GPU_SETUP_RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The device and renderer `prepare_gpu` opens before any mount asks for
    /// one; the first `shared_gpu` takes it.
    static PREPARED_GPU: RefCell<Option<PreparedGpu>> = const { RefCell::new(None) };
    /// What the device's set-up waits on now, for a mount whose first frame
    /// is late (`watch_first_frame`).
    static GPU_STEP: Cell<GpuStep> = const { Cell::new(GpuStep::Queued) };
    /// Mounts presenting a desktop, which applications' notifications wake.
    static DESKTOPS: RefCell<std::collections::BTreeSet<u64>> =
        const { RefCell::new(std::collections::BTreeSet::new()) };
}

/// Schedule a frame for every desktop: an application posted or withdrew a
/// notification, or the Activity pane's code arrived. Runs inside
/// application code or a future, so it reaches the mounts only through the
/// event loop.
pub(crate) fn wake_desktops() {
    let ids = DESKTOPS.with_borrow(Clone::clone);
    LOOP.with_borrow(|proxy| {
        if let Some(proxy) = proxy {
            for id in ids {
                let _ = proxy.send_event(id);
            }
        }
    });
}

/// Code fetched on first use arrived (search, spelling, hyphenation): lay
/// out and draw every mount again, so that what it changes shows. Runs from
/// the loader's promise, outside any mount; a mount that is busy anyway
/// gets a frame through the event loop.
pub(crate) fn refresh_mounts() {
    let ids = MOUNTS.with_borrow(|mounts| mounts.keys().copied().collect::<Vec<_>>());
    for id in ids {
        let refreshed = with_mount(id, |m| {
            m.frame.relayout();
            m.pending = true;
            m.window.request_redraw();
            Ok(())
        });
        if refreshed.is_err() {
            LOOP.with_borrow(|proxy| {
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(id);
                }
            });
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches the page passes in its JSON config"
)]
struct Config {
    /// One application to mount on its own; absent (or `desktop`) mounts
    /// the session desktop.
    #[serde(default)]
    app: Option<String>,
    /// Authenticated model-service access for this mount only.
    #[serde(default)]
    models: Option<crate::host_services::ModelConfig>,
    /// Explicit administration grant, independent of model inference.
    #[serde(default)]
    provider_management: Option<crate::host_services::ProviderManagementConfig>,
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
    /// Render within `WebKit`'s GPU memory: a smaller canvas backing store and
    /// scratch budget (`SurfaceCaps::CONSERVATIVE`).
    #[serde(default)]
    conservative_gpu: bool,
    /// The page takes the desktop's invitations (`mountDesktop({onInvite})`).
    #[serde(default)]
    invite: bool,
    /// The name a desktop seeded for this mount gives its workspace.
    #[serde(default)]
    workspace_name: Option<String>,
    /// The host of the room shown (`example.org`), which the bar names
    /// beside the workspace; absent on the person's own desktop.
    #[serde(default)]
    host: Option<String>,
    /// The deployment's About links (`{notify, source, community}`, from
    /// the web host's `entry.json`); one left empty is not offered.
    #[serde(default)]
    about: tilo_desktop::about::AboutLinks,
}

impl Config {
    fn provider_admin(&self) -> Result<ui::models::ProviderAdministration, JsValue> {
        self.provider_management
            .as_ref()
            .map(crate::host_services::ProviderManagementConfig::connect)
            .transpose()
            .map(Option::unwrap_or_default)
            .map_err(error)
    }

    fn models(&self) -> Result<ui::models::Models, JsValue> {
        self.models
            .as_ref()
            .map(crate::host_services::ModelConfig::connect)
            .transpose()
            .map(Option::unwrap_or_default)
            .map_err(error)
    }
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
/// and full pipeline compilation while the first stayed resident. In `WebKit`
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
        self.failure.lock_recover().clone()
    }

    fn detach(&self, id: u64) -> bool {
        let mut mounts = self.mounts.lock_recover();
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
    drop(gpu);
}

struct GpuSetup {
    id: u64,
    generation: u64,
    window: Arc<Window>,
    caps: SurfaceCaps,
}

/// Bounded exact publication request; never a presence or action invocation.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticPresentationTarget {
    app: String,
    instance: u64,
    record: String,
    resource: String,
    action: String,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent per-mount flags, each read on its own"
)]
struct Mount {
    metrics: RefCell<crate::frame_metrics::FrameMetrics>,
    /// Which statuses carry the semantics tree: unchanged trees are omitted,
    /// content-only changes are throttled, focus and structure changes go out
    /// at once.
    semantics: RefCell<crate::semantics_status::SemanticsGate>,
    window: Arc<Window>,
    canvas: web_sys::HtmlCanvasElement,
    frame: FrameHarness,
    media: ui::headless::host_media::MediaLane,
    store: Rc<durable_store::MemoryStore>,
    /// Every package record owner the mount opened, flushed together.
    stores: Rc<RefCell<Vec<RecordStore>>>,
    /// The single application's record; `None` for the desktop.
    checkout: Option<RecordCheckout>,
    desktop: Option<Desktop>,
    gpu: Option<Gpu>,
    pending: bool,
    background_retry: Option<ui::time::Instant>,
    feedback_retry: Option<ui::time::Instant>,
    viewport: ViewportState,
    caps: SurfaceCaps,
    hidden: bool,
    generation: u64,
    frames: u64,
    /// Protection sampled only after the corresponding GPU output presents.
    presented_capture_protected: bool,
    presentation: std::cell::Cell<bool>,
    semantic_targets: Vec<SemanticPresentationTarget>,
    status: Function,
    images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    /// The window each loader draws for ([`crate::page_content`]).
    image_owners: crate::page_content::ImageOwners,
    /// Content-addressed image requests whose bytes the page does not hold
    /// yet, by loader and URL: asked of the room, handed to the page once
    /// they are here.
    blob_waiting: RefCell<Vec<(usize, String)>>,
    /// The image requests the page is decoding, by loader and URL. Dropping
    /// one (the mount closing) settles its request as failed.
    image_tickets: RefCell<HashMap<(usize, String), ui::image_store::ImageCompletionTicket>>,
    error: Option<String>,
    /// The last composed scene exceeded `scene_within_limits` (reported once).
    oversized_scene: bool,
    /// The peer this mount replicates through, and whether a peer's change
    /// reached it since the last frame.
    peer: Option<u32>,
    replica_changed: Rc<std::cell::Cell<bool>>,
    /// The focus presence last reported to the page.
    focus_presence: RefCell<Option<serde_json::Value>>,
    /// The last `livePlace` the page was told.
    live_place: RefCell<Option<serde_json::Value>>,
    /// Calls into this mount that can change what its semantics tree
    /// shows without a frame saying so: every call but a redraw, a wheel
    /// and the idle scheduler ([`with_mount_quietly`]).
    entries: u64,
    /// [`Self::entries`] when the last semantics tree was built.
    tree_entries: std::cell::Cell<u64>,
    /// The root rect of the last semantics tree built, which a status
    /// whose tree was left unbuilt still reports.
    tree_root_rect: std::cell::Cell<[f64; 4]>,
}

struct Desktop {
    handle: tilo_desktop_surface::DesktopHandle,
    shared: crate::desktop::Shared,
    persistence: crate::desktop::Persistence,
    /// Navigation commands name the mount generation they were issued for.
    generation: String,
    /// What the desktop asked of the page since its last status.
    requests: crate::shell::HostRequests,
    /// Who is here and what they are doing, as the page reports it.
    here: tilo_desktop::here::Here,
    /// Who this page writes as, to find their own steps.
    author: String,
    /// The name and colour each person who opened a window here had when
    /// last seen, by user root: their windows still name them once they
    /// left.
    people: RefCell<HashMap<String, crate::desktop::PersonIdentity>>,
}

impl Desktop {
    /// Tell the surface who holds each window this page's person may not
    /// close or move, on a desktop shared through a room. Whether that
    /// changed anything.
    fn refresh_holders(&self, in_room: bool) -> bool {
        let holders = if in_room {
            let here = self.here.borrow();
            let mut people = self.people.borrow_mut();
            let mut person = |user: &str| match here.person(user) {
                Some(seen) => {
                    people.insert(user.to_owned(), seen.clone());
                    seen
                }
                None => people.get(user).cloned().unwrap_or_else(|| {
                    (
                        tilo_desktop::here::guest_label(),
                        ui::presence::user_color(user),
                    )
                }),
            };
            self.handle.read(|tree| {
                let room = tree
                    .workspaces
                    .first()
                    .map(|workspace| workspace.name.clone())
                    .unwrap_or_default();
                self.shared.holders(tree, &self.author, &room, &mut person)
            })
        } else {
            HashMap::new()
        };
        self.handle.set_holders(holders)
    }
}

impl Mount {
    /// Put who is here on the windows they are in. Remote carets clear a
    /// member's registrations as they move, so this runs after each.
    fn apply_here(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let tree = desktop.handle.read(Clone::clone);
        let people = crate::shell::here_people(&desktop.here, &desktop.shared, &tree);
        desktop
            .here
            .borrow_mut()
            .register_windows(&people, self.frame.presence_mut());
        self.frame.relayout();
        self.pending = true;
        self.window.request_redraw();
    }

    fn service_owner_models(&mut self) {
        self.feedback_retry = None;
        if let Some(desktop) = &self.desktop
            && let Some(feedback) = desktop.shared.feedback.borrow().as_ref()
        {
            let _ = feedback.pump();
        }
        let p = crate::background_work::Presentation {
            hidden: self.hidden,
            suspended: self.viewport.suspended(),
            gpu_ready: self.gpu.is_some(),
        };
        let mut attempted = false;
        let result = crate::background_work::run(p, || {
            let report = self.frame.service_pending_owner_models_report();
            attempted = report.attempted;
            report.result.map(|()| report.completed)
        });
        if attempted {
            self.frame.request_layout();
            self.pending = true;
            self.save_desktop();
            if let Some(peer) = self.peer {
                crate::replication::publish(peer);
            }
        }
        match result {
            Ok(_) => self.background_retry = None,
            Err(error) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "owner model service retained for retry: {error:?}"
                )));
                self.background_retry =
                    ui::time::now().checked_add(ui::time::Duration::from_millis(50));
            }
        }
    }

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
    crate::host_services::install(Arc::new(wake_desktops));
    let event_loop = EventLoop::<u64>::with_user_event().build().map_err(error)?;
    let proxy = event_loop.create_proxy();
    LOOP.with_borrow_mut(|p| *p = Some(proxy.clone()));
    event_loop.spawn_app(Runner);
    Ok(proxy)
}

fn admit_runtime() -> Result<(), JsValue> {
    SERVICING
        .with(crate::mount_service_custody::reject_reentry)
        .map_err(|_| error("the runtime is busy (an owned mount callback)"))
}
fn with_mount<R>(
    id: u64,
    run: impl FnOnce(&mut Mount) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
    with_mount_quietly(id, |m| {
        m.entries = m.entries.wrapping_add(1);
        run(m)
    })
}

/// [`with_mount`] for a call that changes nothing its next frame does not
/// account for: a redraw, a wheel, the idle scheduler. Every other call
/// counts, so a status after it builds the semantics tree.
fn with_mount_quietly<R>(
    id: u64,
    run: impl FnOnce(&mut Mount) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
    if crate::crash::stopped() {
        return Err(error(crate::crash::STOPPED));
    }
    SERVICING.with(|servicing| {
        MOUNTS.with(|mounts| {
            let mut lease = crate::mount_service_custody::Lease::take(mounts, servicing, id)
                .map_err(|_| error("the runtime is busy (an owned mount callback)"))?
                .ok_or_else(|| error("closed mount"))?;
            run(lease.mount().map_err(|_| error("closed mount"))?)
        })
    })
}
fn defer_event(event: DeferredEvent) -> Option<DeferredEvent> {
    SERVICING.with(|servicing| {
        DEFERRED_EVENTS
            .with(|events| crate::mount_service_custody::admit_or_defer(servicing, events, event))
    })
}
#[derive(Clone)]
enum DeferredEvent {
    User(u64),
    Window(WindowId, WindowEvent),
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
    admit_runtime()?;
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

/// Load the code of application `app` when the bundle fetches it on
/// demand, before a page mounts it alone; the session desktop loads its
/// apps itself as their windows open. Resolves `false` for an application
/// the page does not have or whose code failed to load.
#[wasm_bindgen]
pub async fn load_application(app: String) -> bool {
    match app.as_str() {
        "desktop" | "scratch" => true,
        app => {
            ui::application::registry::load(app).await.is_some()
                || crate::desktop::application(app).is_some()
        }
    }
}

/// Close one mount and return its remaining writes (`[{key, bytes, actor,
/// revision}]`) for the page to commit and acknowledge.
///
/// # Errors
///
/// Propagates a failure to serialize the remaining writes.
#[wasm_bindgen]
pub fn close(id: u64) -> Result<JsValue, JsValue> {
    admit_runtime()?;
    SERVICING.with(|servicing| MOUNTS.with(|mounts| {
        let mut lease = crate::mount_service_custody::Lease::take(mounts, servicing, id)
            .map_err(|_| error("the runtime is busy (an owned mount callback)"))?;
        REQUESTS.with_borrow_mut(|r| r.remove(&id));
        DESKTOPS.with_borrow_mut(|desktops| desktops.remove(&id));
        let Some(lease) = lease.as_mut() else { return Ok(js_sys::Array::new().into()); };
        let mut mount = lease.take_mount().map_err(|_| error("closed mount"))?;
    if let Some(peer) = mount.peer {
        crate::replication::detach(peer, id);
    }
    mount.semantic_targets.clear();
    mount.presentation.set(false);
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
    }))
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
    admit_runtime()?;
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
impl Runner {
    fn replay_deferred(&mut self, event_loop: &ActiveEventLoop) {
        let pending = REPLAYING
            .with(|active| {
                SERVICING.with(|servicing| {
                    DEFERRED_EVENTS.with(|events| {
                        crate::mount_service_custody::replay_snapshot(
                            active,
                            servicing,
                            events,
                            |event| match event {
                                DeferredEvent::User(id) => self.user_event(event_loop, id),
                                DeferredEvent::Window(id, event) => {
                                    self.window_event(event_loop, id, event);
                                }
                            },
                        )
                    })
                })
            })
            .unwrap_or(false);
        if pending && let Some(proxy) = LOOP.with_borrow(Clone::clone) {
            // One later turn drains the retained tail; id 0 retains its existing
            // terminal meaning only while STOP is set.
            let _ = proxy.send_event(0);
        }
    }
}

impl ApplicationHandler<u64> for Runner {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, id: u64) {
        let Some(DeferredEvent::User(id)) = defer_event(DeferredEvent::User(id)) else {
            return;
        };
        if id == 0 && STOP.with_borrow(Option::is_some) {
            event_loop.exit();
            return;
        }
        if id == 0 {
            self.replay_deferred(event_loop);
            return;
        }
        if REQUESTS.with_borrow(|requests| requests.contains_key(&id)) {
            let result = SERVICING.with(|servicing| {
                let _construction = crate::mount_service_custody::Construction::enter(servicing, id)
                    .map_err(|_| error("the runtime is busy (mount construction)"))?;
                let Some(request) = REQUESTS.with_borrow_mut(|requests| requests.remove(&id)) else { return Ok(()); };
                let status = request.status.clone();
                if let Err(e) = begin(event_loop, id, request) {
                    emit(&status, &serde_json::json!({"phase":"failed","error":e.as_string().unwrap_or_else(||format!("{e:?}"))}));
                }
                Ok::<(), JsValue>(())
            });
            if let Err(error) = result {
                web_sys::console::warn_1(&error);
                DEFERRED_EVENTS.with_borrow_mut(|events| events.push(DeferredEvent::User(id)));
            }
            self.replay_deferred(event_loop);
            return;
        }
        // Owner model work shows in the frame it asks for, which judges it.
        let result = with_mount_quietly(id, |m| {
            m.service_owner_models();
            if let Some(failure) = m.gpu.as_ref().and_then(Gpu::failure) {
                m.error = Some(failure);
                m.publish(id);
            } else {
                m.window.request_redraw();
            }
            Ok(())
        });
        if let Err(error) = result {
            web_sys::console::warn_1(&error);
        }
        self.replay_deferred(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(DeferredEvent::Window(window_id, event)) =
            defer_event(DeferredEvent::Window(window_id, event))
        else {
            return;
        };
        let id = MOUNTS.with_borrow(|mounts| {
            mounts
                .iter()
                .find(|(_, m)| m.window.id() == window_id)
                .map(|(&id, _)| id)
        });
        let Some(id) = id else {
            return;
        };
        // A redraw is the frame itself, and a wheel's effect is that frame's
        // to judge (`Mount::draw`); every other event counts.
        let quiet = matches!(
            event,
            WindowEvent::RedrawRequested | WindowEvent::MouseWheel { .. }
        );
        let handle = |m: &mut Mount| {
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
                    let passes = m.frame.relayout_frames();
                    crate::clipboard::during_input(&m.status, || m.frame.send_host_event(&event));
                    m.metric_end("input", start);
                    if m.frame.relayout_frames() != passes {
                        m.note_measured();
                    }
                    m.pending = true;
                    m.window.request_redraw();
                }
            }
            Ok(())
        };
        let result = if quiet {
            with_mount_quietly(id, handle)
        } else {
            with_mount(id, handle)
        };
        if let Err(error) = result {
            web_sys::console::warn_1(&error);
        }
        self.replay_deferred(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if admit_runtime().is_err() {
            return;
        }
        let now = ui::time::now();
        let mut model_wakes = Vec::new();
        let mut deadline: Option<ui::time::Instant> = None;
        let mut wake =
            |at: ui::time::Instant| deadline = Some(deadline.map_or(at, |old| old.min(at)));
        let ids = MOUNTS.with_borrow(|mounts| mounts.keys().copied().collect::<Vec<_>>());
        for id in ids {
            let _ = with_mount_quietly(id, |m| {
                let feedback_due = m.desktop.as_ref().and_then(|desktop| {
                    desktop
                        .shared
                        .feedback
                        .borrow()
                        .as_ref()
                        .and_then(PrivateFeedback::next_wake_deadline)
                });
                if let Some(next) = feedback_due {
                    let due = *m.feedback_retry.get_or_insert(next);
                    if due <= now {
                        model_wakes.push(id);
                    } else {
                        wake(due);
                    }
                } else {
                    m.feedback_retry = None;
                }
                if m.frame.has_pending_owner_models() {
                    let due = m.background_retry.unwrap_or(now);
                    if due <= now {
                        model_wakes.push(id);
                    } else {
                        wake(due);
                    }
                }
                // A throttled semantics tree is delivered at its deadline even
                // when no further frame is presented.
                let semantics_due = if m.hidden || m.error.is_some() {
                    None
                } else {
                    m.semantics.borrow().due()
                };
                if let Some(due) = semantics_due {
                    let drawable = !m.viewport.suspended() && m.gpu.is_some();
                    if due <= now && m.frame.replayed_draws() && drawable {
                        // The last frame replayed windows instead of drawing
                        // them; the next one draws every window and delivers
                        // the tree, which then needs no second draw.
                        m.pending = true;
                        m.window.request_redraw();
                    } else if due <= now {
                        m.publish(id);
                    } else {
                        wake(due);
                    }
                }
                if m.hidden || m.error.is_some() || m.gpu.is_none() || m.viewport.suspended() {
                    return Ok(());
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
                Ok(())
            });
        }
        if let Some(proxy) = LOOP.with_borrow(Clone::clone) {
            for id in model_wakes {
                let _ = proxy.send_event(id);
            }
        }
        self.replay_deferred(event_loop);
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
    image_owners: crate::page_content::ImageOwners,
    desktop: Option<Desktop>,
}

enum Root {
    Component(Box<ui::application::AppComponent>),
    Desktop(Box<crate::shell::BrowserShell>),
}

/// Trusted Rust startup handoff, separate from deserialized mount configuration.
/// The certified peer must already exist. This never creates a peer or joins.
pub(crate) fn register_feedback_startup(
    peer: u32,
    startup: tilo_desktop::feedback::transport::Startup,
) -> Result<(), String> {
    let device = startup
        .reporter()
        .device_public_key()
        .ok_or("feedback reporter has no certified device")?;
    if crate::replication::device_of(peer).as_deref() != Some(hex::encode(device).as_str()) {
        return Err("feedback reporter does not own this certified peer".into());
    }
    FEEDBACK_STARTUPS.with_borrow_mut(|startups| {
        if startups.contains_key(&peer) {
            return Err("feedback startup already registered".into());
        }
        startups.insert(peer, startup);
        Ok(())
    })
}

pub(crate) fn forget_feedback_startup(peer: u32) {
    FEEDBACK_STARTUPS.with_borrow_mut(|startups| {
        startups.remove(&peer);
    });
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
    let dyn_history_store = Rc::clone(&dyn_store);
    let wake = proxy()?;
    let (mut surface, handle, shared, persistence) = crate::desktop::mount(
        crate::desktop::Inputs {
            feedback: config.peer.and_then(|peer| {
                FEEDBACK_STARTUPS.with_borrow_mut(|startups| startups.remove(&peer))
            }),
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
            models: config.models()?,
            provider_admin: config.provider_admin()?,
            peer: config.peer,
        },
        dyn_store,
        config.apps.as_deref(),
        config.workspace_name.as_deref(),
        Arc::new(move || {
            let _ = wake.send_event(id);
        }),
    )
    .map_err(error)?;
    surface.set_history_source(Box::new(crate::history::PageHistory::new(
        &config.room,
        config.peer,
        config.author(),
        Rc::clone(&dyn_history_store),
        shared.clone(),
        Rc::clone(&persistence.history),
    )));
    surface.set_notifications(persistence.notifications.clone());
    if let Some(inbox) = crate::host_services::notification_inbox() {
        surface.set_app_notifications(inbox);
    }
    DESKTOPS.with_borrow_mut(|desktops| desktops.insert(id));
    let generation = format!("mount-{id}");
    let requests = crate::shell::HostRequests::default();
    let room = crate::shell::Room {
        authors: Rc::clone(&shared.authors),
        author: config.author(),
    };
    let here = tilo_desktop::here::Here::default();
    let wake = proxy()?;
    let shell = crate::shell::BrowserShell::new(
        surface,
        handle.clone(),
        generation.clone(),
        crate::shell::HostOffers {
            invite: config.invite,
            mac: config.mac,
            host: config.host.clone(),
            about: config.about.clone(),
        },
        Rc::clone(&requests),
        crate::shell::ChromeSources {
            here: Rc::clone(&here),
            shared: shared.clone(),
            room,
            wake: Arc::new(move || {
                let _ = wake.send_event(id);
            }),
        },
    );
    Ok(Presentation {
        root: Root::Desktop(Box::new(shell)),
        stores: Rc::clone(&shared.stores),
        checkout: None,
        images: Rc::clone(&shared.images),
        image_owners: Rc::clone(&shared.image_owners),
        desktop: Some(Desktop {
            handle,
            shared,
            persistence,
            generation,
            requests,
            here,
            author: config.author(),
            people: RefCell::default(),
        }),
    })
}

fn present_component(
    definition: &'static ui::application::ApplicationDefinition,
    config: &Config,
    dyn_store: Rc<dyn durable_store::DurableStore>,
    permissions: Rc<RefCell<PermissionContext>>,
) -> Result<Presentation, JsValue> {
    if definition.app_id == tilo_desktop::feedback::PACKAGE {
        return present_private_feedback(config);
    }
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
        Some(existing)
            if definition.keeps_room_record()
                && existing == RecordId::package_primary().as_str() =>
        {
            (
                Some(RecordCheckout::package_primary(
                    definition.record_kind.clone(),
                )),
                None,
            )
        }
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
        None => definition.unsaved_target(&record).map_err(error)?,
    };
    let views = Rc::new(
        InstanceViewStore::with_store(dyn_store, &config.device, &config.room).map_err(error)?,
    );
    let restore = views
        .load(config.instance)
        .map_err(error)?
        .map_or(RestoreInput::None, RestoreInput::Local);
    let inputs = HostContextInputs {
        records: record.clone(),
        views,
        device_id: config.device.clone(),
        instance_id: config.instance,
        permissions,
        author: UserId(config.author()),
        action_context: tilo_actions::ActionContext::default(),
    };
    let mut context = match named_new {
        Some(id) => HostContext::prepare_new_record(definition, inputs, id),
        None => HostContext::prepare(definition, inputs, &target),
    }
    .map_err(error)?;
    context.provide_models(config.models()?);
    context.provide_content(crate::page_content());
    crate::host_services::provide_provider_admin(&mut context, config.provider_admin()?)
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
    let image_owners = crate::page_content::ImageOwners::default();
    crate::page_content::own_loaders(
        &image_owners,
        0..loaders.borrow().len(),
        definition.app_id,
        &checkout.id,
    );
    Ok(Presentation {
        root: Root::Component(Box::new(component)),
        stores: Rc::new(RefCell::new(vec![record])),
        checkout: Some(checkout),
        images: loaders,
        image_owners,
        desktop: None,
    })
}

fn present_private_feedback(config: &Config) -> Result<Presentation, JsValue> {
    let component =
        tilo_desktop::feedback::unconfigured(&config.device, &config.author(), config.instance)
            .map_err(error)?;
    Ok(Presentation {
        root: Root::Component(Box::new(component)),
        stores: Rc::default(),
        checkout: None,
        images: Rc::default(),
        image_owners: crate::page_content::ImageOwners::default(),
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
    // Apps can measure shared controls while constructing their views. Publish
    // the verified browser fonts first: WASM has no system fonts to fall back
    // to when those constructor-time renderers shape their labels.
    let system = ui::font_catalog::FontCatalog::product("", "en-US")
        .load_browser_startup_bytes(|name| {
            Reflect::get(&fonts, &name.into())
                .ok()
                .filter(|v| !v.is_undefined())
                .map(|v| Uint8Array::new(&v).to_vec())
        })
        .map_err(error)?;
    let text_renderer = ui::TextRenderer::with_browser_font_system(system);
    let definition = definition(config.app.as_deref())?;
    let store = crate::storage::load(&config.profile, &config.device, &hydrated)?;
    let dyn_store: Rc<dyn durable_store::DurableStore> = store.clone();
    let permissions = Rc::new(RefCell::new(PermissionContext::member(
        config.author(),
        collab_types::role::Role::Owner,
    )));
    let Presentation {
        root,
        stores,
        checkout,
        images,
        image_owners,
        desktop,
    } = match definition {
        Some(definition) => {
            present_component(definition, &config, dyn_store, Rc::clone(&permissions))?
        }
        None => present_desktop(id, &config, dyn_store, Rc::clone(&permissions))?,
    };
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
    if let Some(desktop) = &desktop {
        frame
            .attach_owner_wake_queue(desktop.shared.owner_wakes.clone())
            .map_err(|e| error(format!("{e:?}")))?;
    }
    frame.set_input_scale(dpr).map_err(error)?;
    // Every input asks for a redraw on the next animation frame, which
    // lays out and presents: wheel notches, keys, composed text and pointer
    // moves outpace frames, so each is dispatched at once and laid out
    // there, once, not again inside the page's event handler.
    frame.set_coalesce_input_frames(true);
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
                author: config.author(),
                permissions,
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
        media: ui::headless::host_media::MediaLane::default(),
        store,
        stores,
        checkout,
        desktop,
        gpu: None,
        pending: true,
        background_retry: None,
        feedback_retry: None,
        viewport: ViewportState::with_caps(observed, caps),
        caps,
        hidden: false,
        generation: 1,
        frames: 0,
        presented_capture_protected: true,
        presentation: std::cell::Cell::new(false),
        semantic_targets: Vec::new(),
        status: status.clone(),
        images,
        image_owners,
        blob_waiting: RefCell::default(),
        image_tickets: RefCell::default(),
        error: None,
        oversized_scene: false,
        peer: config.peer,
        replica_changed,
        focus_presence: RefCell::new(None),
        live_place: RefCell::new(None),
        entries: 0,
        tree_entries: std::cell::Cell::new(0),
        tree_root_rect: std::cell::Cell::new([0.0; 4]),
    };
    MOUNTS.with_borrow_mut(|m| m.insert(id, mount));
    emit(&status, &serde_json::json!({"phase":"gpu-pending","id":id}));
    initialize_gpu(id, 1, window, caps);
    wasm_bindgen_futures::spawn_local(watch_first_frame(id));
    Ok(())
}

/// Where graphics start-up stands for a mount that has not drawn yet, as the
/// page names it (`gpuStep` in the status).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GpuStep {
    /// Not started: behind the runtime's own work or another mount's set-up.
    Queued,
    /// Asking the browser for a WebGPU adapter and device.
    Device,
    /// The browser compiling the renderer's pipelines.
    Pipelines,
    /// Configuring the canvas on the device.
    Surface,
    /// Set up; its first frame not presented yet.
    FirstFrame,
}

impl GpuStep {
    const fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Device => "device",
            Self::Pipelines => "pipelines",
            Self::Surface => "surface",
            Self::FirstFrame => "first-frame",
        }
    }

    const fn waiting_on(self) -> &'static str {
        match self {
            Self::Queued => "waiting for graphics set-up to start",
            Self::Device => "waiting for the WebGPU device",
            Self::Pipelines => "waiting for the renderer's pipelines to compile",
            Self::Surface => "waiting to configure the canvas",
            Self::FirstFrame => "waiting for the first frame to present",
        }
    }
}

/// How long a mount may be visible without presenting its first frame
/// before it fails, naming the step graphics start-up is stuck on, so the
/// page says it stopped and offers to try again instead of waiting in
/// silence. A first frame takes about 2 s on a GPU and 15-35 s under a
/// software rasteriser (`SwiftShader` in CI).
const FIRST_FRAME_BOUND_MS: u32 = 60_000;
/// How often [`watch_first_frame`] looks.
const FIRST_FRAME_TICK_MS: u32 = 1_000;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(handler: &Function, ms: u32) -> JsValue;
}

pub(crate) async fn sleep_ms(ms: u32) {
    let timer = js_sys::Promise::new(&mut |resolve, _| {
        let _ = set_timeout(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(timer).await;
}

/// Watch mount `id` until its first frame is out: say each step graphics
/// start-up moves to (`gpuStep`), and fail the mount with that step as its
/// error once it has been visible for [`FIRST_FRAME_BOUND_MS`] without a
/// frame. Time hidden (a background tab draws nothing) does not count.
async fn watch_first_frame(id: u64) {
    let mut visible_ms: u32 = 0;
    let mut said = None;
    loop {
        sleep_ms(FIRST_FRAME_TICK_MS).await;
        if !await_runtime_admission().await {
            return;
        }
        let watching = with_mount_quietly(id, |m| {
            if m.frames > 0 || m.error.is_some() {
                return Ok(false);
            }
            let step = if m.gpu.is_some() {
                GpuStep::FirstFrame
            } else {
                GPU_STEP.get()
            };
            if said != Some(step) {
                said = Some(step);
                emit(
                    &m.status,
                    &serde_json::json!({"phase":"gpu-pending","id":id,"gpuStep":step.name()}),
                );
            }
            if m.hidden || m.viewport.suspended() {
                return Ok(true);
            }
            visible_ms = visible_ms.saturating_add(FIRST_FRAME_TICK_MS);
            if visible_ms < FIRST_FRAME_BOUND_MS {
                return Ok(true);
            }
            m.error = Some(format!(
                "WebGPU presented no first frame in {} s, {}",
                FIRST_FRAME_BOUND_MS / 1000,
                step.waiting_on()
            ));
            m.publish(id);
            Ok(false)
        });
        if !matches!(watching, Ok(true)) {
            return;
        }
    }
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

/// A device opened, and vello's pipelines compiled on it, ahead of the first
/// surface.
enum PreparedGpu {
    /// Opening; the promise resolves once `Ready` replaced this.
    Opening(js_sys::Promise),
    Ready(Box<Result<(ui::gpu::RenderContext, vello::Renderer), String>>),
}

/// Open the WebGPU device and compile the renderer's pipelines now, while the
/// page opens the workspace, rather than once the first mount's canvas
/// exists: the first surface then only configures its canvas on a device
/// that is ready. Without a mount nothing else happens; the first mount
/// takes what this opened, and one that comes before it is done waits for
/// it. Does nothing once a device exists or is being opened.
///
/// The entry page calls this once a host let it in, never before: the
/// browser does not cancel the compiles of a page torn down meanwhile, and
/// the next page's pipelines queue behind them on its compile threads. The conservative GPU profile (Safari)
/// does not call it; its renderer starts only once the page's load settled.
#[wasm_bindgen]
pub fn prepare_gpu() {
    let busy = PREPARED_GPU.with_borrow(Option::is_some)
        || SHARED_GPU.with_borrow(Option::is_some)
        || GPU_SETUP_RUNNING.get();
    if busy || crate::crash::stopped() {
        return;
    }
    let mut resolve = None;
    let opened = js_sys::Promise::new(&mut |done, _| resolve = Some(done));
    PREPARED_GPU.set(Some(PreparedGpu::Opening(opened)));
    wasm_bindgen_futures::spawn_local(async move {
        let ready = open_gpu().await;
        PREPARED_GPU.set(Some(PreparedGpu::Ready(Box::new(ready))));
        if let Some(done) = resolve {
            let _ = done.call0(&JsValue::NULL);
        }
    });
}

async fn open_gpu() -> Result<(ui::gpu::RenderContext, vello::Renderer), String> {
    let mut cx = ui::gpu::RenderContext::new();
    GPU_STEP.set(GpuStep::Device);
    let index = cx.device(None).await.ok_or("no WebGPU device")?;
    let handle = cx.devices.get(index).ok_or("WebGPU device missing")?;
    GPU_STEP.set(GpuStep::Pipelines);
    let renderer = new_renderer(&handle.device).await?;
    Ok((cx, renderer))
}

/// What `prepare_gpu` opened, once it is done; `None` when it was never
/// called or its device was already taken.
async fn take_prepared_gpu() -> Option<Result<(ui::gpu::RenderContext, vello::Renderer), String>> {
    let opening = PREPARED_GPU.with_borrow(|prepared| match prepared {
        Some(PreparedGpu::Opening(opened)) => Some(opened.clone()),
        _ => None,
    });
    if let Some(opened) = opening {
        let _ = wasm_bindgen_futures::JsFuture::from(opened).await;
    }
    match PREPARED_GPU.take()? {
        PreparedGpu::Ready(ready) => Some(*ready),
        // Only the GPU setup queue takes it, one setup at a time.
        PreparedGpu::Opening(_) => None,
    }
}

/// Asynchronous pipeline creation: the browser compiles vello's pipelines in
/// parallel off its GPU process's main thread. Created synchronously they
/// hold that thread for the whole compile (seconds each under a software
/// rasteriser), and a page waiting on the GPU process (one tearing down its
/// previous WebGPU context, a frame, a screenshot) waits with it.
///
/// Only the pipelines a default full frame dispatches are awaited (13 of
/// 24): the rest (sparse fine, the indirect path and large path-tag scan
/// stages, the GPU draw prefix, clip reduce) start compiling once the first
/// frame is out (`start_deferred_pipelines_async` in `render`). A frame that
/// needs one sooner creates it there and then, so nothing is ever drawn
/// without its pipeline.
async fn new_renderer(device: &wgpu::Device) -> Result<vello::Renderer, String> {
    vello::Renderer::new_async_deferring(
        device,
        vello::RendererOptions {
            antialiasing_support: vello::AaSupport::area_only(),
            ..Default::default()
        },
        vello::PipelineDeferral::Unused,
    )
    .await
    .map_err(|e| e.to_string())
}

/// The shared device and renderer, created with the first surface (or taken
/// from `prepare_gpu`).
#[expect(
    clippy::await_holding_refcell_ref,
    reason = "other mounts only try_borrow the device and skip a frame meanwhile"
)]
async fn shared_gpu(
    window: &Arc<Window>,
    size: Viewport,
    caps: SurfaceCaps,
) -> Result<(Rc<RefCell<SharedGpu>>, vello::util::RenderSurface<'static>), String> {
    let live = SHARED_GPU
        .with_borrow(Clone::clone)
        .filter(|s| s.try_borrow().is_ok_and(|s| s.failure().is_none()));
    if let Some(shared) = live {
        GPU_STEP.set(GpuStep::Surface);
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
    // A preparation that failed is tried again here, so its error is this
    // surface's.
    let (mut cx, prepared) = match take_prepared_gpu().await {
        Some(Ok((cx, renderer))) => (cx, Some(renderer)),
        Some(Err(_)) | None => (ui::gpu::RenderContext::new(), None),
    };
    // Without a prepared device, the surface's creation opens one.
    GPU_STEP.set(if prepared.is_some() {
        GpuStep::Surface
    } else {
        GpuStep::Device
    });
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
    let mut renderer = match prepared {
        // Prepared on the context's first device, which presents to any
        // canvas on WebGPU.
        Some(renderer) if surface.dev_id == 0 => renderer,
        _ => {
            GPU_STEP.set(GpuStep::Pipelines);
            new_renderer(&handle.device).await?
        }
    };
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
        for &id in mounts.lock_recover().iter() {
            let _ = p.send_event(id);
        }
    };
    let (lost, lost_mounts, lost_proxy) = (Arc::clone(&failure), Arc::clone(&mounts), p.clone());
    handle
        .device
        .set_device_lost_callback(move |reason, message| {
            *lost.lock_recover() = Some(format!(
                "WebGPU device lost: {reason:?}: {message}; call recover"
            ));
            wake(&lost_proxy, &lost_mounts);
        });
    let (uncaptured, error_mounts) = (Arc::clone(&failure), Arc::clone(&mounts));
    handle.device.on_uncaptured_error(Arc::new(move |e| {
        *uncaptured.lock_recover() = Some(format!("WebGPU error: {e}; call recover"));
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

/// Leases are synchronous and never cross await. A nested future retains its
/// payload until the current callback returns rather than reading an absent
/// mount or treating Busy as a stale generation.
async fn await_runtime_admission() -> bool {
    while admit_runtime().is_err() {
        if crate::crash::stopped() {
            return false;
        }
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
    }
    !crate::crash::stopped()
}

async fn set_up_gpu(setup: GpuSetup) {
    let GpuSetup {
        id,
        generation,
        window,
        caps,
    } = setup;
    if !await_runtime_admission().await {
        return;
    }
    let current = with_mount(id, |m| Ok(m.generation == generation)).unwrap_or(false);
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
    if !await_runtime_admission().await {
        return;
    }
    let installed = with_mount(id, |m| {
        if m.generation != generation { return Ok(false); }
        match result {
            Ok((shared, surface)) => {
                if let Ok(s) = shared.try_borrow() {
                    s.mounts
                        .lock_recover()
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
                Ok(true)
            }
            Err(e) => {
                m.error = Some(format!("WebGPU unavailable: {e}"));
                m.publish(id);
                Ok(false)
            }
        }
    }).unwrap_or(false);
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

    /// With frame metrics on, name the widgets that made the layout pass
    /// just run re-solve sizes, when it did without recomposing: a pass
    /// that only moved laid-out content (a pan) names none.
    fn note_measured(&self) {
        let stats = self.frame.last_relayout();
        if stats.measured && !stats.rebuilt && self.metrics.borrow().enabled() {
            let requesters = self.frame.layout_measure_requesters();
            self.metrics.borrow_mut().measured(requesters);
        }
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
        // Host wakes are consumed only after acquiring a surface. A skipped
        // acquisition must retain them for the next redraw.
        let desktop_woken = self.desktop.as_ref().is_some_and(|desktop| {
            desktop
                .shared
                .woken
                .load(std::sync::atomic::Ordering::Acquire)
        });
        self.pending |= desktop_woken || self.replica_changed.get();
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
        let held = self.desktop.as_ref().is_some_and(|desktop| {
            desktop
                .handle
                .set_viewport_width(f64::from(viewport.logical_size().0));
            desktop.refresh_holders(self.peer.is_some())
        });
        // Desktop commands and peer changes need an update even when the
        // harness itself is idle. Let a scheduled poll consume them first:
        // eagerly relayouting here and then polling animated widgets runs
        // the same frame pipeline twice before one presentation.
        let mut host_changed = held;
        if let Some(desktop) = self.desktop.as_ref().filter(|d| d.shared.take_wake()) {
            let wakers = desktop.shared.take_wakers();
            if let Some(reasons) = &mut reasons {
                if wakers.is_empty() {
                    reasons.push("desktop wake".into());
                }
                reasons.extend(wakers.iter().map(|app| format!("wake: {app}")));
            }
            if desktop.shared.take_arrived() {
                desktop.shared.retry_waiting(&desktop.handle);
            }
            host_changed = true;
        }
        // A peer's change merged into an open record.
        if self.replica_changed.replace(false) {
            if let Some(reasons) = &mut reasons {
                reasons.push("replica change".into());
            }
            host_changed = true;
        }
        // A frame whose semantics will be built draws every window: one it
        // replayed instead (a pan frame may) would have to be drawn again
        // before its tree could be read.
        let tree_due = host_changed
            || self.entries != self.tree_entries.get()
            || !self.semantics.borrow().would_defer_unbuilt(ui::time::now());
        self.frame.set_pan_replay(!tree_due);
        // Poll only after applying the new viewport, avoiding an old-size
        // layout merely to discover work.
        let stage = self.metric_start();
        let polled = self.frame.poll_frame().map_err(|e| format!("{e:?}"))?;
        let stats = self.frame.last_relayout();
        self.metric_end(
            match (polled, stats.rebuilt, stats.measured) {
                (false, ..) => "poll_frame:idle",
                (true, true, _) => "poll_frame:rebuilt",
                (true, false, true) => "poll_frame:measured",
                (true, false, false) => "poll_frame:applied",
            },
            stage,
        );
        if polled {
            self.note_measured();
        }
        // A frame that only re-applied positions, with no host change and
        // no call since the last tree that the frame cannot see, moved
        // laid-out content and nothing else: its tree's structure and focus
        // are the last one's.
        let moved_only = polled && !stats.rebuilt && !stats.measured && !host_changed;
        if host_changed && !polled {
            let stage = self.metric_start();
            self.frame
                .relayout_at(viewport.logical_size())
                .map_err(|e| format!("{e:?}"))?;
            self.metric_end("wake_relayout", stage);
        }
        let stage = self.metric_start();
        self.save_desktop();
        self.metric_end("save_desktop", stage);
        if let Some(peer) = self.peer {
            let stage = self.metric_start();
            crate::replication::publish(peer);
            self.metric_end("replicate", stage);
        }
        let stage = self.metric_start();
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
        self.metric_end("scene", stage);
        let stage = self.metric_start();
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
        ui::gpu::draw_target_to_surface(
            &handle.device,
            &mut encoder,
            &gpu.surface,
            &output.texture,
        );
        handle.queue.submit([encoder.finish()]);
        // The first frame is out: the pipelines it did not need compile now.
        shared
            .renderer
            .start_deferred_pipelines_async(&handle.device);
        drop(shared_guard);
        output.present();
        self.presented_capture_protected = self.frame.capture_protected();
        self.metric_end("render", stage);
        self.viewport.did_present();
        self.pending = false;
        self.frames = self.frames.saturating_add(1);
        if let Some(reasons) = reasons {
            self.metrics.borrow_mut().presented(reasons);
        }
        let stage = self.metric_start();
        self.presentation.set(true);
        self.publish_frame(0, moved_only);
        self.presentation.set(false);
        self.metric_end("publish", stage);
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

    /// Where this person is on the desktop, for the room's presence:
    /// `{focus, place}`, the record of the focused window (the desktop's
    /// own record where no window is focused or it has none yet) and the
    /// strip shown. `None` for a single application.
    fn live_place_value(&self) -> Option<serde_json::Value> {
        let desktop = self.desktop.as_ref()?;
        let (focus, place) = desktop.handle.read(|tree| {
            let place = tree.active_workspace().and_then(|workspace| {
                tilo_desktop_surface::navigation::active_strip(tree, workspace)
            });
            let focus = tilo_desktop_surface::navigation::active_route(tree)
                .and_then(|route| desktop.shared.live_record_of(tree, route.tab));
            (focus, place)
        });
        let (app, record) = focus.unwrap_or_else(|| {
            (
                crate::desktop::DESKTOP_PACKAGE.to_owned(),
                crate::replication::DESKTOP_RECORD.to_owned(),
            )
        });
        Some(serde_json::json!({
            "focus": {"app": app, "record": record},
            "place": place.map(|strip| strip.0.to_string()),
        }))
    }

    fn publish(&mut self, id: u64) {
        self.publish_frame(id, false);
    }

    /// Whether a frame's tree stays unbuilt: only a frame that moved
    /// laid-out content may leave it, while the gate would defer it or the
    /// frame replayed windows its tree would need drawn again.
    fn tree_unbuilt(&self, moved_only: bool, ime: Option<&ui::ime::ImeWindowState>) -> bool {
        if !moved_only || self.error.is_some() {
            false
        } else if self.entries != self.tree_entries.get() {
            // What kept a moved frame's tree from waiting, for attribution.
            self.metric_end("tree_built:mount_call", self.metric_start());
            false
        } else if self
            .semantics
            .borrow_mut()
            .defer_unbuilt(ime, ui::time::now())
        {
            true
        } else if self.frame.replayed_draws()
            && self
                .semantics
                .borrow_mut()
                .hold_for_next_frame(ime, ui::time::now())
        {
            // The window ended on a frame that replayed windows instead of
            // drawing them: the next frame draws every window and delivers
            // the tree, which then needs no second draw.
            self.metric_end("tree_held:replayed", self.metric_start());
            true
        } else {
            self.metric_end("tree_built:gate", self.metric_start());
            false
        }
    }

    /// Publish the status of a presented frame. `moved_only` says the frame
    /// only moved laid-out content ([`Self::draw`]); then, while the gate
    /// would defer a content-only change anyway, the tree is not built at
    /// all, and the gate's deadline delivers it as it then is.
    fn publish_frame(&mut self, id: u64, moved_only: bool) {
        let ime_window = self.frame.ime_window();
        let ime = ime_window.as_ref().map(|i| {
            serde_json::json!({"x":i.x,"y":i.y,"width":i.width,"height":i.height,"purpose":format!("{:?}",i.content.purpose),"hints":i.content.html()})
        });
        let unbuilt = self.tree_unbuilt(moved_only, ime_window.as_ref());
        let (decision, root_rect) = if unbuilt {
            (None, self.tree_root_rect.get())
        } else {
            // A window a pan frame replayed instead of drawing has not noted
            // where it now paints what its semantics name.
            let stage = self.metric_start();
            self.frame.settle_replayed_draws();
            self.metric_end("settle_replayed_draws", stage);
            let stage = self.metric_start();
            let semantics = self.frame.inspection();
            self.metric_end("inspection", stage);
            self.tree_entries.set(self.entries);
            self.tree_root_rect.set(semantics.root.rect);
            // The semantics tree is the bulk of every status; the host keeps
            // the last tree it received, so an unchanged tree is left out and
            // content-only changes while typing are throttled.
            let stage = self.metric_start();
            let decision = self.semantics.borrow_mut().decide(
                &semantics,
                ime_window.as_ref(),
                ui::time::now(),
            );
            self.metric_end("semantics_decide", stage);
            (Some(decision), semantics.root.rect)
        };
        let applied = self.viewport.applied();
        let value = serde_json::json!({
            "phase": if self.error.is_some() { "failed" } else { "presented" },
            "id": id,
            "error": self.error,
            "frames": self.frames,
            "captureProtected": self.presented_capture_protected,
            "presentation": self.presentation.get(),
            "componentInstance": self.frame.component_instance_id(),
            "semanticLoci": self.presentation.get().then(|| self.semantic_targets.iter().map(|target| {
                let rect = if self.desktop.is_none()
                    && self.checkout.as_ref().is_some_and(|c| c.id.as_str() == target.record)
                {
                    self.frame.published_action_locus(target.instance, &target.app, &target.record,
                        &tilo_actions::ActionAddress::new(&target.resource, &target.action))
                        .map(|r| [r.x0, r.y0, r.width(), r.height()])
                } else { None };
                serde_json::json!({"target": target, "rect": rect})
            }).collect::<Vec<_>>()),
            "gpuGeneration": self.generation,
            "layoutGeneration": self.frame.relayout_frames(),
            "logicalSize": applied.logical_size(),
            "dpr": applied.dpr,
            "surfaceSize": self.gpu.as_ref().map(|g| [g.surface.config.width, g.surface.config.height]),
            "rootRect": root_rect,
            "record": self.checkout.as_ref().map(|checkout| checkout.id.as_str()),
            "desktopRevision": self.desktop.as_ref().map(|d| d.handle.revision()),
            "searchOpen": self.desktop.as_ref().is_some_and(|d| d.handle.search_open()),
            // The page titles its tab after it.
            "workspaceName": self.desktop.as_ref().and_then(|d| d.handle.read(|tree| tree.active_workspace().map(|w| w.name.clone()))),
            "hostRequests": self.desktop.as_ref().map(|d| std::mem::take(&mut *d.requests.borrow_mut())).filter(|r| !r.is_empty()),
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
        let place = self.live_place_value();
        if *self.live_place.borrow() != place
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "livePlace".into(),
                place.clone().unwrap_or(serde_json::Value::Null),
            );
            *self.live_place.borrow_mut() = place;
        }
        let mut text = value.to_string();
        if let Some(crate::semantics_status::SemanticsDecision::Deliver { .. }) = decision {
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

/// Publish component aliases through the canonical native publication namespace.
/// Existing `component_actions` retains its domain-address compatibility.
///
/// # Errors
/// Refuses closed mounts and unavailable component publications.
#[wasm_bindgen]
pub fn component_publications(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let resources = m
            .frame
            .published_bound_resources_of(None)
            .map_err(|e| door_error(&e))?;
        serde_json::to_string(&tilo_actions::door::survey(&resources)).map_err(error)
    })
}

/// Native catalogue requests, answered by the page's existing certified peer.
/// Shares the retained request collector with the local media lane.
/// # Errors
/// Refuses closed mounts or unencodable requests.
#[wasm_bindgen]
pub fn component_catalogue_requests(id: u64) -> Result<String, JsValue> {
    // The page asks every frame; taking the requests changes nothing the
    // tree shows (an answer arrives through a call of its own).
    with_mount_quietly(id, |m| {
        let requests = m.media.take_catalogue_requests(&mut m.frame);
        serde_json::to_string(&requests).map_err(error)
    })
}

/// Deliver one certified catalogue answer to this live native app mount.
/// The app fences the originating request, current resource and timeout.
/// # Errors
/// Refuses closed mounts, oversized or non-catalogue answers.
#[wasm_bindgen]
pub fn component_catalogue_response(id: u64, response: &str) -> Result<(), JsValue> {
    if response.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return Err(error("catalogue response exceeds its wire budget"));
    }
    let message: tilo_actions::remote::RemoteActionMessage =
        serde_json::from_str(response).map_err(error)?;
    if !matches!(
        message,
        tilo_actions::remote::RemoteActionMessage::CatalogueResponse { .. }
            | tilo_actions::remote::RemoteActionMessage::Receipt(_)
    ) {
        return Err(error("not a catalogue response or receipt"));
    }
    with_mount(id, |m| {
        // This only reports the originating command's actual system transfer.
        // The browser's own copy command still refuses absent/expired permission.
        crate::clipboard::during_input(&m.status, || {
            m.frame
                .handle_collaboration_event(&ui::collaboration::CollabEvent::RemoteAction {
                    message,
                });
        });
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Register/remove an exact read-only target for the next successful presentation.
/// Requests are local observations and do not establish authenticated admission.
///
/// # Errors
/// Refuses malformed/oversized targets, desktops and more than 32 queued targets.
#[wasm_bindgen]
pub fn request_component_locus(id: u64, target: &str, observe: bool) -> Result<(), JsValue> {
    if target.len() > 17_000 {
        return Err(error("semantic target exceeds size bound"));
    }
    let target: SemanticPresentationTarget = serde_json::from_str(target).map_err(error)?;
    if target.instance > 9_007_199_254_740_991
        || [
            &target.app,
            &target.record,
            &target.resource,
            &target.action,
        ]
        .into_iter()
        .any(|s| s.is_empty() || s.len() > 4096)
    {
        return Err(error("invalid semantic target"));
    }
    with_mount(id, |m| {
        if m.desktop.is_some() {
            return Err(error("semantic presentation requires a single-app mount"));
        }
        m.semantic_targets.retain(|t| t != &target);
        if observe {
            if m.semantic_targets.len() >= 32 {
                return Err(error("semantic target capacity exceeded"));
            }
            m.semantic_targets.push(target);
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
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
            ui::host_operation::ResourceOperationStart::Completed(output) => {
                serde_json::json!({"state": "completed", "output": output})
            }
            ui::host_operation::ResourceOperationStart::Accepted => {
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
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                serde_json::to_string(&d.handle.snapshot(&d.generation, d.shared.catalogue()))
                    .map_err(error)
            })
            .transpose()
    })
}

/// Where the desktop shows each record right now: `[{app, record, rect:
/// [x0, y0, x1, y1]}]` for the active tab of every window in the shown
/// strip, in the surface's CSS pixels, each record named as the room's live
/// layer names a member's focus. The page draws a collaborator's pointer
/// inside the window of the record it is focused on. `None` for a single
/// application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_record_windows(id: u64) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let shown = d.handle.shown_tabs();
                let windows: Vec<serde_json::Value> = d.handle.read(|tree| {
                    shown
                        .into_iter()
                        .filter_map(|(tab, rect)| {
                            let (app, record) = d.shared.live_record_of(tree, tab)?;
                            Some(serde_json::json!({
                                "app": app,
                                "record": record,
                                "rect": [rect.x0, rect.y0, rect.x1, rect.y1],
                            }))
                        })
                        .collect()
                });
                serde_json::to_string(&windows).map_err(error)
            })
            .transpose()
    })
}

/// Where the desktop shows each member's legacy window right now: `[{key,
/// rect: [x0, y0, x1, y1], focused}]` for windows whose active tab shows one
/// in the shown strip, in the surface's CSS pixels, `focused` for the
/// desktop's focused window; empty while search covers the desktop. `None`
/// for a single application.
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
                    let focused = d.handle.read(|tree| {
                        tilo_desktop_surface::navigation::active_route(tree).map(|route| route.tab)
                    });
                    d.handle
                        .shown_tabs()
                        .into_iter()
                        .filter_map(|(tab, rect)| {
                            Some(serde_json::json!({
                                "key": d.shared.peer_window_of(tab)?,
                                "rect": [rect.x0, rect.y0, rect.x1, rect.y1],
                                "focused": focused == Some(tab),
                            }))
                        })
                        .collect()
                };
                serde_json::to_string(&windows).map_err(error)
            })
            .transpose()
    })
}

/// Focus the desktop window showing the legacy window `key`: the person
/// clicked into it, which the page handles, not the desktop.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_focus_peer_window(id: u64, key: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let Some(tab) = desktop.shared.peer_window_tab(key) else {
            return Ok(());
        };
        if desktop.handle.focus_tab(tab) {
            desktop.shared.wake();
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// List the legacy applications the node beside the host runs (`[{id,
/// title}]` JSON, `id` what the node opens): desktop search and the
/// navigation catalogue offer them, and opening one is handed to the page
/// ([`desktop_take_node_app_requests`]).
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_node_apps(id: u64, apps: &str) -> Result<(), JsValue> {
    let apps: Vec<crate::desktop::NodeApp> = serde_json::from_str(apps).map_err(error)?;
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .shared
            .set_node_apps(apps);
        Ok(())
    })
}

/// Offer what only the page can do for the person (`[{id, title,
/// subtitle?}]` JSON; in place of earlier ones): desktop search lists them
/// beside the desktop's commands, and choosing one reaches the page as a
/// host request `{action: id}`.
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_host_actions(id: u64, actions: &str) -> Result<(), JsValue> {
    let actions: Vec<tilo_desktop_surface::HostAction> =
        serde_json::from_str(actions).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        // An ask for an action no longer offered was answered some other way.
        let mut withdrew = false;
        for gone in desktop.handle.set_host_actions(actions) {
            withdrew |= desktop
                .persistence
                .notifications
                .withdraw_host_action(&gone);
        }
        if withdrew {
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// Show a banner offering a host action (`{action: {id, title}, title,
/// body}` JSON) until the person chooses or dismisses it; choosing it
/// reaches the page as that action.
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_offer_host_action(id: u64, offer: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct Offer {
        action: tilo_desktop_surface::HostAction,
        title: String,
        #[serde(default)]
        body: String,
    }
    let offer: Offer = serde_json::from_str(offer).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop
            .persistence
            .notifications
            .post(offer.action.notice(offer.title, offer.body));
        m.window.request_redraw();
        Ok(())
    })
}

/// Confirm in passing what the page just did out of sight ("Link copied"):
/// a short-lived pill over the desktop, never a notification.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_acknowledge(id: u64, text: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .handle
            .acknowledge(text);
        m.window.request_redraw();
        Ok(())
    })
}

/// The room's host answered the room action `request` the desktop asked for
/// (a `room-action` host request): `answer` is `{"ok": value}` or
/// `{"error": "why"}`.
///
/// # Errors
///
/// Rejects a closed or single-application mount, or an answer of neither
/// form.
#[wasm_bindgen]
pub fn desktop_room_action_answer(id: u64, request: u32, answer: &str) -> Result<(), JsValue> {
    let answer: serde_json::Value = serde_json::from_str(answer).map_err(error)?;
    let answer = match (answer.get("ok"), answer.get("error")) {
        (Some(value), None) => Ok(value.clone()),
        (None, Some(why)) => Err(why.as_str().unwrap_or("refused").to_owned()),
        _ => return Err(error("a room action answer is {ok} or {error}")),
    };
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.shared.room_actions.answer(request, answer);
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// The node applications the person asked to open since the last call (a
/// JSON array of their ids): the page asks the node to open each, and its
/// window arrives as a legacy window.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_take_node_app_requests(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let requests = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .shared
            .take_node_app_requests();
        serde_json::to_string(&requests).map_err(error)
    })
}

/// Hand a painted window (`tilo-paint` stream) one reassembled frame of its
/// stream: a whole page, patches, or an image it references. Returns whether
/// what the window shows changed; the desktop then draws it again.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_peer_window_paint(id: u64, key: &str, frame: &[u8]) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let changed = desktop.shared.paint_page(key).push(frame);
        if changed {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(changed)
    })
}

/// Input the person gave painted windows since the last call, as JSON
/// `[{key, events}]` with `legacy_window::InputEvent`s in page pixels, for
/// the page to send to each stream's host.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_take_peer_window_input(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let input: Vec<serde_json::Value> = desktop
            .shared
            .take_paint_input()
            .into_iter()
            .map(|(key, events)| serde_json::json!({"key": key, "events": events}))
            .collect();
        serde_json::to_string(&input).map_err(error)
    })
}

/// The legacy windows the person closed since the last call (a JSON array
/// of their keys): the page asks their sources to close them, and the
/// desktop does not open them again while they still stream.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_dismissed_peer_windows(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let keys = desktop
            .handle
            .read(|tree| desktop.shared.take_dismissed_peer_windows(tree));
        serde_json::to_string(&keys).map_err(error)
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
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
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
        && crate::desktop::known_application(app).is_none()
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
        m.apply_here();
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Who is in the room's live layer now: `{own: {publisher, label, color},
/// members: [{publisher, kind, label, color, focus, activity}] | null}`,
/// `members` null while the layer is not live. The bar, the People popover
/// and the windows show them; nothing is written to the room.
///
/// # Errors
///
/// Rejects malformed JSON or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_people(id: u64, people: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct People {
        own: Option<tilo_desktop::here::LiveMember>,
        members: Option<Vec<tilo_desktop::here::LiveMember>>,
    }
    let people: People = serde_json::from_str(people).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop
            .here
            .borrow_mut()
            .set_members(people.own, people.members)
        {
            // The bar reads the strips' counts in its update pass, which a
            // bare redraw skips: without the wake it would paint them only
            // on its next other change, such as the clock's minute.
            desktop.shared.wake();
            m.apply_here();
        }
        Ok(())
    })
}

/// Who the desktop shows as here, as its People list does: `[{id, label,
/// agent}]`, one row per person however many devices they are here from,
/// never this page's own person. `None` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_here_people(id: u64) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let tree = d.handle.read(Clone::clone);
                let people: Vec<serde_json::Value> = crate::shell::here_people(&d.here, &d.shared, &tree)
                    .into_iter()
                    .map(|person| serde_json::json!({"id": person.id, "label": person.label, "agent": person.agent}))
                    .collect();
                serde_json::to_string(&people).map_err(error)
            })
            .transpose()
    })
}

/// The host's answer to `/here` (`peer.here`): each person's line.
///
/// # Errors
///
/// Rejects malformed JSON or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_here(id: u64, lines: &str) -> Result<(), JsValue> {
    let lines: Vec<tilo_desktop::here::HostLine> = serde_json::from_str(lines).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop
            .here
            .borrow_mut()
            .set_lines(lines, crate::shell::now_ms())
        {
            m.apply_here();
        }
        Ok(())
    })
}

/// How many spectators follow the room without taking part.
///
/// # Errors
///
/// Rejects a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_watching(id: u64, watching: u32) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop.here.borrow_mut().set_watching(watching) {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// What this page's person is doing, for the page to post to `/here`:
/// `{window: hex | null, share, actions: [{id, action, resource, app,
/// at_ms}]}`. `window` is the focused window's tab id; `actions` are their own steps in the active workspace's records since
/// `since_ms` (titles only), and empty unless they share an automatic
/// status (`share`).
///
/// # Errors
///
/// Rejects a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_here_tick(id: u64, since_ms: f64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let tree = desktop.handle.read(Clone::clone);
        let focused = tree.active_workspace().and_then(|workspace| {
            // A person who has not moved yet is shown the first strip, as
            // the composition draws it.
            let strip = tree
                .active_strip
                .get(&workspace.id)
                .and_then(|id| workspace.strips.iter().find(|s| s.id == *id))
                .or_else(|| workspace.strips.first())?;
            let group = tree
                .active_group_for(strip.id)
                .and_then(|g| strip.window_groups.iter().find(|w| w.id == g))
                .or_else(|| strip.window_groups.first())?;
            group.tabs.iter().find(|tab| tab.id == group.active_tab)
        });
        let window = focused.map(|tab| format!("{:x}", tab.id));
        let share = desktop.here.borrow().sharing() == ui::StatusSharing::Automatic;
        let actions = if share {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a page clock in milliseconds; negative clamps to zero"
            )]
            let since = since_ms.max(0.0) as u64;
            crate::history::own_recent_actions(&desktop.shared, &tree, &desktop.author, since)
        } else {
            Vec::new()
        };
        serde_json::to_string(&serde_json::json!({
            "window": window,
            "share": share,
            "actions": actions,
        }))
        .map_err(error)
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
        m.apply_here();
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
    // The page persists after every presented status; storing what is
    // written changes nothing the tree shows.
    with_mount_quietly(id, |m| {
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
        // A GPU that failed drawing at full resolution recovers one size
        // smaller, rather than failing the same way again.
        if m.error.take().is_some() {
            m.caps = m.caps.after_gpu_failure();
            m.viewport.set_caps(m.caps);
        }
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
    // Takes what the last shaping asked to fetch; the tree is the same.
    with_mount_quietly(id, |m| {
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

/// Image URLs the application asked for, as `[{loader, url, maxDimension}]`.
/// The page fetches and decodes each with the browser's own image decoders,
/// no larger than `maxDimension` on either side, and hands the pixels back
/// through [`complete_asset`].
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn asset_requests(id: u64) -> Result<String, JsValue> {
    // The page asks after every presented status; taking the image requests
    // changes nothing the tree shows (a loaded image wakes its own frame).
    with_mount_quietly(id, |m| {
        let mut tickets = m.image_tickets.borrow_mut();
        let mut loaders = m.images.borrow_mut();
        let mut wanted: Vec<(usize, String)> = m.blob_waiting.take();
        for (loader, images) in loaders.iter_mut().enumerate() {
            wanted.extend(images.take_requests().into_iter().map(|url| (loader, url)));
        }
        let mut requests = Vec::new();
        for (loader, url) in wanted {
            // Content the page lacks is asked of the room; the request waits
            // here until it has arrived.
            if !crate::page_content::image_ready(&m.image_owners, loader, &url) {
                m.blob_waiting.borrow_mut().push((loader, url));
                continue;
            }
            let Some(ticket) = loaders
                .get_mut(loader)
                .and_then(|images| images.reserve(&url))
            else {
                continue;
            };
            requests.push(serde_json::json!({
                "loader": loader,
                "url": url,
                "maxDimension": ticket.max_dimension(),
            }));
            tickets.insert((loader, url), ticket);
        }
        // What the page asked for goes to the room now, not on the next
        // frame an idle page may never draw.
        if !m.blob_waiting.borrow().is_empty()
            && let Some(peer) = m.peer
        {
            crate::replication::ask_content(peer);
        }
        serde_json::to_string(&requests).map_err(error)
    })
}

/// The bytes of a content-addressed image request ([`asset_requests`]
/// hands one out only once they are here), for the page to decode.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn blob_asset(id: u64, loader: usize, url: &str) -> Result<Option<Vec<u8>>, JsValue> {
    with_mount_quietly(id, |m| {
        Ok(crate::page_content::image_bytes(
            &m.image_owners,
            loader,
            url,
        ))
    })
}

/// Deliver a requested image the page decoded: `width` × `height` pixels of
/// unpremultiplied 8-bit RGBA, or `None` when it could not be fetched or
/// decoded. Resolves whether the image was accepted.
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
    width: u32,
    height: u32,
    rgba: Option<Vec<u8>>,
) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let Some(ticket) = m
            .image_tickets
            .borrow_mut()
            .remove(&(loader, url.to_owned()))
        else {
            return Ok(false);
        };
        let complete = m
            .images
            .borrow_mut()
            .get_mut(loader)
            .ok_or_else(|| error("unknown image loader"))?
            .complete_decoded(
                &ticket,
                rgba.map(|rgba| ui::image_store::DecodedImage {
                    rgba,
                    width,
                    height,
                }),
            );
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
pub fn visible(id: u64, visible: bool) -> Result<(), JsValue> {
    admit_runtime()?;
    crate::host_services::set_hidden(!visible);
    MOUNTS.with_borrow_mut(|mounts| {
        if let Some(m) = mounts.get_mut(&id) {
            m.hidden = !visible;
            if visible {
                m.pending = true;
                m.window.request_redraw();
            }
        }
    });
    Ok(())
}

/// Names applications requested through `ui::host_data` since the last
/// call, as a JSON array, for the page to fetch from the bundle's `data/`.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn data_requests() -> Result<String, JsValue> {
    admit_runtime()?;
    serde_json::to_string(&ui::host_data::take_requests()).map_err(error)
}

/// Supply requested data, verified by the page against the bundle's
/// manifest, and relayout every mount so the requester sees it.
#[wasm_bindgen]
pub fn install_data(name: &str, bytes: Vec<u8>) -> Result<(), JsValue> {
    admit_runtime()?;
    ui::host_data::install(name, bytes);
    relayout_all();
    Ok(())
}

/// Report that requested data cannot be fetched.
#[wasm_bindgen]
pub fn data_failed(name: &str, reason: &str) -> Result<(), JsValue> {
    admit_runtime()?;
    ui::host_data::fail(name, reason);
    relayout_all();
    Ok(())
}

fn relayout_all() {
    let ids = MOUNTS.with_borrow(|mounts| mounts.keys().copied().collect::<Vec<_>>());
    for id in ids {
        let result = with_mount(id, |mount| {
            mount.frame.relayout();
            mount.pending = true;
            mount.window.request_redraw();
            Ok(())
        });
        if let Err(error) = result {
            web_sys::console::warn_1(&error);
        }
    }
}

/// Opt-in bounded diagnostic capture; disabled by default and never persisted.
#[wasm_bindgen]
pub fn configure_frame_metrics(enabled: bool) -> Result<(), JsValue> {
    admit_runtime()?;
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut() {
            mount.metrics.borrow_mut().configure(enabled);
            if enabled {
                mount.frame.begin_profiling();
            } else {
                mount.frame.end_profiling();
            }
        }
    });
    Ok(())
}

/// Drain captured stage timings and per-pass layout profiles, per mount.
///
/// # Errors
///
/// Propagates a serialization failure.
#[wasm_bindgen]
pub fn take_frame_metrics() -> Result<String, JsValue> {
    admit_runtime()?;
    MOUNTS.with_borrow_mut(|mounts| {
        let samples: Vec<_> = mounts
            .iter_mut()
            .map(|(id, mount)| {
                let mut metrics = mount.metrics.borrow_mut();
                serde_json::json!({"mount": id, "samples": metrics.take(), "dropped": metrics.take_dropped(), "requesters": metrics.take_requesters(), "measuredBy": metrics.take_measured_by(), "layoutPasses": mount.frame.take_profile_samples(), "lastRelayout": format!("{:?}", mount.frame.last_relayout())})
            })
            .collect();
        serde_json::to_string(&samples).map_err(error)
    })
}

/// The shared Chat controls' local media requests, scoped to retained owners.
///
/// # Errors
///
/// Rejects a closed mount or a failure to serialize its requests.
#[wasm_bindgen]
pub fn take_media_requests(id: u64) -> Result<String, JsValue> {
    with_mount(id, |mount| {
        serde_json::to_string(&mount.media.take(&mut mount.frame)).map_err(error)
    })
}

/// Complete one exact retained component request; late replies are refused.
///
/// # Errors
///
/// Rejects malformed snapshots and closed mounts.
#[wasm_bindgen]
pub fn complete_media_request(id: u64, ticket: u64, response: &str) -> Result<bool, JsValue> {
    let result: Result<ui::native_media::Snapshot, String> =
        serde_json::from_str(response).map_err(error)?;
    if result.as_ref().is_ok_and(|snapshot| !snapshot.valid()) {
        return Err(error("invalid local media snapshot"));
    }
    with_mount(id, |mount| {
        let completed = mount.media.complete(&mut mount.frame, ticket, result);
        if completed {
            mount.window.request_redraw();
        }
        Ok(completed)
    })
}
