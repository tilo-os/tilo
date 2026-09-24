//! One page-lifetime Winit loop. JS owns DOM/IndexedDB; this owns the real component and GPU.
use js_sys::{Function, Reflect, Uint8Array};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
};
use ui::{
    application::{ComponentRunnerInputs, HostContext, LaunchTarget, RestoreInput},
    application_view::InstanceViewStore,
    headless::FrameHarness,
    permissions::{Actor, PermissionContext, Role, UserId},
    portable_storage::{PendingWrite, PortableStorage},
    record::{RecordCheckout, RecordId, RecordOwners},
};
use wasm_bindgen::{JsCast, prelude::*};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys},
    window::{Window, WindowId},
};

thread_local! {
    static LOOP: RefCell<Option<EventLoopProxy<u64>>> = const { RefCell::new(None) };
    static MOUNTS: RefCell<HashMap<u64, Mount>> = RefCell::new(HashMap::new());
    static REQUESTS: RefCell<HashMap<u64, Request>> = RefCell::new(HashMap::new());
    static CACHES: RefCell<HashMap<(String,String), PortableStorage>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u64> = const { RefCell::new(1) };
    static CLOSED: RefCell<crate::close_recovery::ClosedComponents> = RefCell::new(crate::close_recovery::ClosedComponents::default());
    static STOP: RefCell<Option<Function>> = const { RefCell::new(None) };
}
#[derive(serde::Deserialize)]
struct Config {
    app: String,
    profile: String,
    device: String,
    room: String,
    author: String,
    instance: u64,
    record: Option<String>,
    peer: Option<u32>,
    #[serde(default)]
    mac: bool,
    /// Milliseconds edits may wait before publication while typing continues.
    /// Defaults to `PUBLICATION_WINDOW_MS`; zero publishes every changed frame.
    #[serde(default)]
    publication_window_ms: Option<u64>,
    /// Milliseconds between semantics deliveries while the tree keeps changing
    /// without a focus or structure change. Defaults to
    /// `semantics_status::SEMANTICS_WINDOW_MS`; zero sends every changed tree.
    #[serde(default)]
    semantics_window_ms: Option<u64>,
}
/// Default publication window. Changed records publish at most once per window
/// while edits keep arriving; close, hide, focus loss, IME end and peer rebind
/// publish immediately, so only a hard tab kill inside the window loses edits.
/// Requires a native session peer with checkpoint cadence (2026-09-19): older
/// natives re-published a multi-megabyte checkpoint per delta, and with sparser
/// deltas the relay dropped them for backpressure and replayed the room on
/// reconnect. Hosts and benchmarks can override with `publication_window_ms`.
const PUBLICATION_WINDOW_MS: u64 = 100;
fn publication_schedule(
    window_ms: Option<u64>,
) -> RefCell<crate::admitted_records::PublicationSchedule> {
    RefCell::new(crate::admitted_records::PublicationSchedule::new(
        std::time::Duration::from_millis(window_ms.unwrap_or(PUBLICATION_WINDOW_MS)),
    ))
}
fn semantics_gate(window_ms: Option<u64>) -> RefCell<crate::semantics_status::SemanticsGate> {
    RefCell::new(crate::semantics_status::SemanticsGate::new(
        std::time::Duration::from_millis(
            window_ms.unwrap_or(crate::semantics_status::SEMANTICS_WINDOW_MS),
        ),
    ))
}
use crate::storage_key::DiskKey;
use crate::viewport::{Viewport, ViewportState};
struct Request {
    canvas: web_sys::HtmlCanvasElement,
    config: Config,
    fonts: JsValue,
    hydrated: JsValue,
    status: Function,
}
struct Gpu {
    cx: ui::gpu::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    renderer: vello::Renderer,
    failure: Arc<Mutex<Option<String>>>,
}
struct Mount {
    metrics: RefCell<crate::frame_metrics::FrameMetrics>,
    published_records: std::cell::RefCell<crate::admitted_records::PublishedRecords>,
    publication: RefCell<crate::admitted_records::PublicationSchedule>,
    /// Decides which statuses carry the semantics tree: unchanged trees are
    /// omitted, content-only changes are throttled, focus and structure
    /// changes go out at once.
    semantics: RefCell<crate::semantics_status::SemanticsGate>,
    peer_suspended: bool,
    peer: Option<u32>,
    desktop: Option<crate::desktop_host::DesktopHandle>,
    record: Option<crate::desktop_host::OwnedRecord>,
    window: Arc<Window>,
    canvas: web_sys::HtmlCanvasElement,
    frame: FrameHarness,
    storage: PortableStorage,
    gpu: Option<Gpu>,
    pending: bool,
    viewport: ViewportState,
    hidden: bool,
    generation: u64,
    frames: u64,
    status: Function,
    writes: HashMap<u64, PendingWrite>,
    images: Vec<ui::image_store::HostImageLoader>,
    error: Option<String>,
    /// The last composed scene exceeded `scene_within_limits` (reported once).
    oversized_scene: bool,
}
/// Glyphs one frame may encode. A full 4K screen of small text is far below
/// this; only unculled off-screen content reaches it.
const MAX_SCENE_GLYPHS: usize = 400_000;
/// Encoded scene bytes one frame may hand to the renderer (before glyph
/// outlines are resolved, which multiplies glyph cost).
const MAX_SCENE_BYTES: usize = 128 << 20;
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
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn emit(f: &Function, v: serde_json::Value) {
    emit_text(f, &v.to_string());
}
fn emit_text(f: &Function, text: &str) {
    let _ = f.call1(&JsValue::NULL, &JsValue::from_str(text));
}
fn proxy() -> Result<EventLoopProxy<u64>, JsValue> {
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|panic| {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "Browser runtime panic: {panic}"
            )));
        }));
    });
    if let Some(p) = LOOP.with_borrow(|p| p.clone()) {
        return Ok(p);
    }
    crate::clipboard::install();
    qdoc::blobstore::set_store(Arc::new(qdoc::blobstore::MemoryBlobStore::default()));
    let event_loop = EventLoop::<u64>::with_user_event().build().map_err(error)?;
    let proxy = event_loop.create_proxy();
    LOOP.with_borrow_mut(|p| *p = Some(proxy.clone()));
    event_loop.spawn_app(Runner);
    Ok(proxy)
}
#[wasm_bindgen]
pub fn mount(
    canvas: web_sys::HtmlCanvasElement,
    config: &str,
    fonts: JsValue,
    hydrated: JsValue,
    status: Function,
) -> Result<u64, JsValue> {
    if STOP.with_borrow(|s| s.is_some()) {
        return Err(error("terminal shutdown is still pending"));
    }
    let p = proxy()?;
    let id = NEXT.with_borrow_mut(|next| {
        let id = *next;
        *next += 1;
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
#[wasm_bindgen]
pub fn close(id: u64) -> Result<String, JsValue> {
    REQUESTS.with_borrow_mut(|r| r.remove(&id));
    MOUNTS.with_borrow_mut(|m| {
        let Some(mut mount)=m.remove(&id) else {return Ok("[]".into());};
        mount.publish_with(id, true);
        mount.frame.close_components();
        let reports=mount.frame.take_component_close_reports();
        for record in mount.owned_records(){let _=record.store.try_flush();}
        let pending=mount.storage.pending_writes();
        // Bytes stay in the shared cache until the host's transaction completes.
        let writes=pending.iter().map(write_json).collect::<Vec<_>>();
        emit(&mount.status,serde_json::json!({"phase":"closed","id":id,"closeReports":reports.len(),"pendingWrites":writes.len()}));
        CLOSED.with_borrow_mut(|closed|closed.retain(id,reports,mount.storage.clone()));
        unconfigure(&mount.canvas);
        mount.gpu.take();
        Ok(serde_json::to_string(&writes).map_err(error)?)
    })
}
fn unconfigure(canvas: &web_sys::HtmlCanvasElement) {
    if let Ok(Some(context)) = canvas.get_context("webgpu") {
        if let Ok(method) =
            Reflect::get(&context, &"unconfigure".into()).and_then(|v| v.dyn_into::<Function>())
        {
            let _ = method.call0(&context);
        }
    }
}
#[wasm_bindgen]
pub fn shutdown(done: Function) -> Result<(), JsValue> {
    if STOP.with_borrow(|s| s.is_some()) {
        return Err(error("shutdown already pending"));
    }
    let ids = MOUNTS.with_borrow(|m| m.keys().copied().collect::<Vec<_>>());
    for id in ids {
        close(id)?;
    }
    if CLOSED.with_borrow(|closed| closed.len() > 0) {
        return Err(error(
            "component close recovery must settle before terminal shutdown",
        ));
    }
    REQUESTS.with_borrow_mut(HashMap::clear);
    STOP.with_borrow_mut(|s| *s = Some(done));
    if let Some(p) = LOOP.with_borrow(|p| p.clone()) {
        p.send_event(0).map_err(error)?;
    } else {
        finish_shutdown();
    }
    Ok(())
}
fn finish_shutdown() {
    LOOP.with_borrow_mut(|p| *p = None);
    // Winit resets its creation guard after LoopExiting returns. The microtask runs after that.
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
        if id == 0 && STOP.with_borrow(|s| s.is_some()) {
            event_loop.exit();
            return;
        }
        if let Some(request) = REQUESTS.with_borrow_mut(|r| r.remove(&id)) {
            let status = request.status.clone();
            if let Err(e) = begin(event_loop, id, request) {
                emit(
                    &status,
                    serde_json::json!({"phase":"failed","error":e.as_string().unwrap_or_else(||format!("{e:?}"))}),
                );
            }
        } else {
            MOUNTS.with_borrow_mut(|mounts| {
                if let Some(m) = mounts.get_mut(&id) {
                    let failure = m
                        .gpu
                        .as_ref()
                        .and_then(|gpu| gpu.failure.lock().unwrap().clone());
                    if let Some(failure) = failure {
                        m.error = Some(failure);
                        m.publish(id);
                    } else {
                        m.window.request_redraw();
                    }
                }
            });
        }
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
                    if m.peer_suspended {
                        return;
                    }
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
                _ => {
                    if m.peer_suspended {
                        return;
                    }
                    let mut event = event;
                    // The old layout is still displayed (stretched by CSS) until
                    // redraw commits the requested viewport. Winit reports the
                    // new physical coordinates, so map pointer/touch back to it.
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
                    if let Some(desktop) = m.desktop.clone() {
                        desktop.finish_local_activation(&mut m.frame);
                    }
                    m.metric_end("input", start);
                    if start.is_some() {
                        if m.frame.last_send_skipped_relayout() {
                            m.metrics.borrow_mut().count("relayout_skipped");
                        }
                        for path in m.frame.take_pointer_redraw_sources() {
                            m.metrics.borrow_mut().note(&path);
                        }
                    }
                    if matches!(event, WindowEvent::Focused(false)) {
                        m.publish_records(id, true);
                    }
                    if start.is_some() {
                        let demand = m.frame.frame_schedule();
                        let mut metrics = m.metrics.borrow_mut();
                        if demand.needs_redraw {
                            metrics.count("input_demand.redraw");
                        }
                        if demand.needs_animation_frame {
                            metrics.count("input_demand.animation");
                        }
                        if demand
                            .next_wake_deadline
                            .is_some_and(|wake| wake <= ui::clock::Instant::now())
                        {
                            metrics.count("input_demand.deadline");
                        }
                    }
                    m.pending = true;
                    m.window.request_redraw();
                }
            }
        });
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = ui::clock::Instant::now();
        let mut deadline = None;
        MOUNTS.with_borrow(|mounts| {
            for (&id, m) in mounts.iter() {
                // Deferred publication runs when its window elapses, independently
                // of presentation; otherwise the loop wakes for it.
                // The RefCell borrow must end before publishing re-borrows it mutably.
                let due = if m.peer_suspended {
                    None
                } else {
                    m.publication.borrow().due()
                };
                if let Some(due) = due {
                    if due <= now {
                        let start = m.metric_start();
                        m.publish_records(id, true);
                        m.metric_end("publish_flush", start);
                    } else {
                        deadline =
                            Some(deadline.map_or(due, |old: ui::clock::Instant| old.min(due)));
                    }
                }
                // A throttled semantics tree is delivered at its deadline even
                // when no further frame is presented, so the accessibility DOM
                // never lags the screen by more than one window.
                let semantics_due = if m.peer_suspended || m.hidden || m.error.is_some() {
                    None
                } else {
                    m.semantics.borrow().due()
                };
                if let Some(due) = semantics_due {
                    if due <= now {
                        let start = m.metric_start();
                        m.publish(id);
                        m.metric_end("semantics_flush", start);
                    } else {
                        deadline =
                            Some(deadline.map_or(due, |old: ui::clock::Instant| old.min(due)));
                    }
                }
                if m.peer_suspended
                    || m.hidden
                    || m.error.is_some()
                    || m.gpu.is_none()
                    || m.viewport.suspended()
                {
                    continue;
                }
                let start = m.metric_start();
                let output = m.frame.frame_schedule();
                m.metric_end("schedule", start);
                if m.pending
                    || m.viewport.needs_present()
                    || output.needs_redraw
                    || output.needs_animation_frame
                {
                    m.window.request_redraw();
                }
                if let Some(wake) = output.next_wake_deadline {
                    if wake <= now {
                        m.window.request_redraw();
                    } else {
                        deadline =
                            Some(deadline.map_or(wake, |old: ui::clock::Instant| old.min(wake)));
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
fn begin(event_loop: &ActiveEventLoop, id: u64, request: Request) -> Result<(), JsValue> {
    let Request {
        canvas,
        config,
        fonts,
        hydrated,
        status,
    } = request;
    if config.app == "desktop" {
        return begin_desktop(event_loop, id, canvas, config, fonts, hydrated, status);
    }
    let definition = match config.app.as_str() {
        "writer" => word_processor::definition(),
        "chat" => chat::definition(),
        _ => return Err(error("unsupported application")),
    };
    let storage = load_cache(&config.profile, &config.device, &hydrated)?;
    let records = RecordOwners::portable(storage.clone())
        .open(definition.app_id, &config.room)
        .map_err(error)?;
    let target = if config.app == "chat" {
        let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
        match records.try_open_record(&checkout) {
            Ok(_) => LaunchTarget::Record {
                checkout,
                reveal: None,
            },
            Err(ui::record::RecordOpenError::MissingRecord) if config.peer.is_none() => {
                LaunchTarget::PrimaryNew
            }
            Err(e) => return Err(error(e)),
        }
    } else if let Some(record) = &config.record {
        LaunchTarget::Record {
            checkout: RecordCheckout::new(
                RecordId::parse(record).ok_or_else(|| error("invalid ordinary record"))?,
                definition.record_kind.clone(),
            ),
            reveal: None,
        }
    } else {
        LaunchTarget::New
    };
    let views = Rc::new(InstanceViewStore::portable(storage.clone(), &config.room).map_err(error)?);
    let restore = views
        .load(config.instance)
        .map_err(error)?
        .map_or(RestoreInput::None, RestoreInput::Local);
    let permissions = Rc::new(RefCell::new(if let Some(peer) = config.peer {
        crate::peer::permissions(peer).map_err(error)?
    } else {
        PermissionContext::from_capabilities(
            Actor::person(Some(Role::Owner), &config.author),
            vec![],
        )
    }));
    let context = HostContext::prepare(
        definition,
        records.clone(),
        views,
        config.device.clone(),
        config.instance,
        permissions,
        UserId(config.author),
        tilo_actions::ActionContext::default(),
        &target,
    )
    .map_err(error)?;
    let loaders = Rc::new(RefCell::new(Vec::new()));
    let destination = loaders.clone();
    let component = ui::ImageStore::with_host_factory_with_max_dimension(
        Rc::new(move |max_dimension| {
            let (image, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
            destination.borrow_mut().push(loader);
            image
        }),
        || definition.instantiate(context, target, restore),
    )
    .map_err(error)?;
    let images = std::mem::take(&mut *loaders.borrow_mut());
    let checkout = component.record().clone();
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
    let dpr = window.scale_factor();
    let wake = proxy()?;
    let mut frame = FrameHarness::from_component_with_wake_notifier(
        component,
        (
            (size.width as f64 / dpr).max(1.0) as u32,
            (size.height as f64 / dpr).max(1.0) as u32,
        ),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::with_browser_font_system(system),
        },
        move || {
            let _ = wake.send_event(id);
        },
    )
    .map_err(error)?;
    frame.set_input_scale(dpr).map_err(error)?;
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(if config.mac {
        ui::platform::Platform::MacOs
    } else {
        ui::platform::Platform::Linux
    }));
    let mount = Mount {
        metrics: Default::default(),
        published_records: Default::default(),
        publication: publication_schedule(config.publication_window_ms),
        semantics: semantics_gate(config.semantics_window_ms),
        peer_suspended: false,
        peer: config.peer,
        desktop: None,
        record: Some(crate::desktop_host::OwnedRecord {
            app: definition.app_id.into(),
            store: records,
            checkout,
        }),
        window: window.clone(),
        canvas,
        frame,
        storage,
        gpu: None,
        pending: true,
        viewport: ViewportState::new(Viewport {
            width: size.width,
            height: size.height,
            dpr,
        }),
        hidden: false,
        generation: 1,
        frames: 0,
        status: status.clone(),
        writes: HashMap::new(),
        images,
        error: None,
        oversized_scene: false,
    };
    MOUNTS.with_borrow_mut(|m| m.insert(id, mount));
    emit(&status, serde_json::json!({"phase":"gpu-pending","id":id}));
    initialize_gpu(id, 1, window);
    Ok(())
}
fn begin_desktop(
    event_loop: &ActiveEventLoop,
    id: u64,
    canvas: web_sys::HtmlCanvasElement,
    config: Config,
    fonts: JsValue,
    hydrated: JsValue,
    status: Function,
) -> Result<(), JsValue> {
    let peer = config
        .peer
        .ok_or_else(|| error("canonical desktop requires an admitted certified peer"))?;
    let state = crate::peer::desktop_state(peer).map_err(error)?;
    if state.room() != config.room {
        return Err(error("desktop room binding mismatch"));
    }
    let admission = crate::desktop::admission(&state).map_err(error)?;
    let storage = load_cache(&config.profile, &config.device, &hydrated)?;
    let permissions = crate::peer::permissions(peer).map_err(error)?;
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
    let dpr = window.scale_factor();
    let wake = proxy()?;
    let (root, desktop) = crate::desktop_host::DesktopRoot::new(
        admission,
        storage.clone(),
        &config.room,
        &config.device,
        &config.author,
        config.instance,
        permissions,
        Arc::new(move || {
            let _ = wake.send_event(id);
        }),
    )
    .map_err(error)?;
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(root),
        (
            (size.width as f64 / dpr).max(1.0) as u32,
            (size.height as f64 / dpr).max(1.0) as u32,
        ),
        ui::Theme::dark(),
        ui::TextRenderer::with_browser_font_system(system),
        UserId(config.author),
    );
    frame.set_input_scale(dpr).map_err(error)?;
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(if config.mac {
        ui::platform::Platform::MacOs
    } else {
        ui::platform::Platform::Linux
    }));
    desktop.sync_global_search(&mut frame);
    let images = std::mem::take(&mut *desktop.images.borrow_mut());
    let mount = Mount {
        metrics: Default::default(),
        published_records: Default::default(),
        publication: publication_schedule(config.publication_window_ms),
        semantics: semantics_gate(config.semantics_window_ms),
        peer_suspended: false,
        peer: Some(peer),
        desktop: Some(desktop),
        record: None,
        window: window.clone(),
        canvas,
        frame,
        storage,
        gpu: None,
        pending: true,
        viewport: ViewportState::new(Viewport {
            width: size.width,
            height: size.height,
            dpr,
        }),
        hidden: false,
        generation: 1,
        frames: 0,
        status: status.clone(),
        writes: HashMap::new(),
        images,
        error: None,
        oversized_scene: false,
    };
    MOUNTS.with_borrow_mut(|mounts| mounts.insert(id, mount));
    emit(&status, serde_json::json!({"phase":"gpu-pending","id":id}));
    initialize_gpu(id, 1, window);
    Ok(())
}
fn initialize_gpu(id: u64, generation: u64, window: Arc<Window>) {
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let size = window.inner_size();
            let mut cx = ui::gpu::RenderContext::new();
            let surface = cx
                .create_surface(
                    window.clone(),
                    size.width.max(1),
                    size.height.max(1),
                    wgpu::PresentMode::Fifo,
                )
                .await
                .map_err(|e| e.to_string())?;
            let handle = &cx.devices[surface.dev_id];
            let renderer = vello::Renderer::new(
                &handle.device,
                vello::RendererOptions {
                    antialiasing_support: vello::AaSupport::area_only(),
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
            let p = proxy().map_err(|e| format!("{e:?}"))?;
            let failure = Arc::new(Mutex::new(None));
            let lost = failure.clone();
            let lost_proxy = p.clone();
            handle
                .device
                .set_device_lost_callback(move |reason, message| {
                    *lost.lock().unwrap() = Some(format!(
                        "WebGPU device lost: {reason:?}: {message}; call recover"
                    ));
                    let _ = lost_proxy.send_event(id);
                });
            let uncaptured = failure.clone();
            handle.device.on_uncaptured_error(Arc::new(move |e| {
                *uncaptured.lock().unwrap() = Some(format!("WebGPU error: {e}; call recover"));
                let _ = p.send_event(id);
            }));
            Ok::<_, String>(Gpu {
                cx,
                surface,
                renderer,
                failure,
            })
        }
        .await;
        let installed = MOUNTS.with_borrow_mut(|mounts|{
            let Some(m)=mounts.get_mut(&id).filter(|m|m.generation==generation) else{return false;};
            match result{Ok(gpu)=>{
                emit(&m.status,serde_json::json!({"initialSurfaceSize":[gpu.surface.config.width,gpu.surface.config.height]}));
                m.gpu=Some(gpu);
                m.viewport.surface_installed();
                m.queue_resize();
                m.pending=true;m.window.request_redraw();
                true
            },Err(e)=>{m.error=Some(format!("WebGPU unavailable: {e}"));m.publish(id);false}}
        });
        // GPU setup completes outside Winit dispatch. Wake an idle loop after
        // releasing the mount borrow so the new generation actually presents.
        if installed {
            if let Ok(wake) = proxy() {
                let _ = wake.send_event(id);
            }
        }
    });
}
impl Mount {
    fn metric_start(&self) -> Option<web_time::Instant> {
        self.metrics.borrow().begin()
    }
    fn metric_end(&self, stage: &'static str, start: Option<web_time::Instant>) {
        self.metrics.borrow_mut().end(stage, start);
    }

    fn owned_records(&self) -> Vec<crate::desktop_host::OwnedRecord> {
        if let Some(desktop) = &self.desktop {
            desktop.records.borrow().values().cloned().collect()
        } else {
            self.record.iter().cloned().collect()
        }
    }
    fn collect_desktop(&mut self) {
        if let Some(desktop) = &self.desktop {
            self.images
                .extend(std::mem::take(&mut *desktop.images.borrow_mut()));
            desktop.sync_global_search(&mut self.frame);
        }
    }

    // ResizeObserver/Winit remains the sole source of viewport notifications.
    // Reading the pair again at redraw also covers scale/size event ordering and
    // changes which occurred while GPU initialization or peer recovery was pending.
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
    fn draw_measured(&mut self) -> Result<(), String> {
        let prepare_start = self.metric_start();
        if self.peer_suspended || self.hidden || self.error.is_some() {
            return Ok(());
        }
        self.queue_resize();
        if self.viewport.suspended() || self.gpu.is_none() {
            return Ok(());
        }
        let viewport = self.viewport.requested();
        self.collect_desktop();
        // Inspect wake flags without rebuilding the old viewport first. Layout
        // and input scale may advance only after acquisition succeeds below.
        let frame = self.frame.frame_schedule();
        self.pending |= self.viewport.needs_present()
            || frame.needs_redraw
            || frame.needs_animation_frame
            || frame
                .next_wake_deadline
                .is_some_and(|wake| wake <= ui::clock::Instant::now());
        if !self.pending {
            return Ok(());
        }
        self.metric_end("draw_prepare", prepare_start);
        let acquire_start = self.metric_start();
        let gpu = self.gpu.as_mut().expect("GPU checked above");
        if let Some(reason) = gpu.cx.devices[gpu.surface.dev_id].loss_monitor().reason() {
            return Err(format!(
                "WebGPU device lost: {reason}; call recover to recreate the renderer"
            ));
        }
        // Configuration changes canvas.width/height on WebGPU, clearing its
        // displayed image. Keep configure -> acquire -> submit -> present in
        // this synchronous redraw callback, with no destructive equal-size resize.
        if self
            .viewport
            .needs_surface_resize(gpu.surface.config.width, gpu.surface.config.height)
        {
            gpu.cx
                .resize_surface(&mut gpu.surface, viewport.width, viewport.height);
        }
        let mut acquired = gpu.surface.surface.get_current_texture();
        if matches!(acquired, wgpu::CurrentSurfaceTexture::Outdated)
            && self.viewport.recover_outdated()
        {
            gpu.cx.configure_surface(&gpu.surface);
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
        let Some(output) = self
            .viewport
            .apply_for_acquired_texture(output, |viewport| {
                self.frame.set_input_scale(viewport.dpr)?;
                self.frame
                    .resize(viewport.logical_size())
                    .map_err(|e| e.to_string())
            })?
        else {
            // Keep both the old input/layout geometry and pending work on a
            // skipped acquisition. Winit coalesces the retry into the next rAF.
            self.window.request_redraw();
            return Ok(());
        };
        // Poll only after applying the new viewport. This avoids an old-size
        // layout merely to discover work, followed by a second resize layout.
        self.metrics
            .borrow_mut()
            .end("draw_acquire_resize", acquire_start);
        let poll_start = self.metrics.borrow().begin();
        if poll_start.is_some() {
            let demand = self.frame.frame_schedule();
            let mut metrics = self.metrics.borrow_mut();
            if demand.needs_redraw {
                metrics.count("draw_demand.redraw");
            }
            if demand.needs_animation_frame {
                metrics.count("draw_demand.animation");
            }
            if demand
                .next_wake_deadline
                .is_some_and(|wake| wake <= ui::clock::Instant::now())
            {
                metrics.count("draw_demand.deadline");
            }
        }
        self.frame.poll_frame().map_err(|e| e.to_string())?;
        if let Some(desktop) = &self.desktop {
            if let Err(error) = desktop.process_global_search(&mut self.frame) {
                desktop.errors.borrow_mut().push(error);
            }
        }
        self.metrics.borrow_mut().end("draw_poll", poll_start);
        let snapshot_start = self.metrics.borrow().begin();
        let frame = self.frame.frame_render_input();
        self.metrics
            .borrow_mut()
            .end("draw_snapshot", snapshot_start);
        // Never hand the renderer a scene it cannot resolve: an oversized
        // encoding panics in vello's resolver (capacity overflow on wasm32)
        // or exceeds WebGPU buffer limits, which took down the whole runtime
        // after very large pastes. Keep the last presented frame, tell the
        // user once, and keep the session (and its durable edits) alive.
        if let Err(reason) = scene_within_limits(frame.scene) {
            if !self.oversized_scene {
                self.oversized_scene = true;
                web_sys::console::error_1(&JsValue::from_str(&reason));
                if let Some(desktop) = &self.desktop {
                    desktop.errors.borrow_mut().push(
                        "This content is too large to display in full; recent edits are kept."
                            .into(),
                    );
                }
            }
            self.pending = false;
            return Ok(());
        }
        self.oversized_scene = false;
        let render_start = self.metrics.borrow().begin();
        let mut physical = vello::Scene::new();
        physical.append(
            frame.scene,
            Some(ui::scale::composite_transform(viewport.dpr)),
        );
        let handle = &gpu.cx.devices[gpu.surface.dev_id];
        gpu.renderer
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
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        gpu.surface.blitter.copy(
            &handle.device,
            &mut encoder,
            &gpu.surface.target_view,
            &output.texture.create_view(&Default::default()),
        );
        handle.queue.submit([encoder.finish()]);
        output.present();
        self.metrics
            .borrow_mut()
            .end("draw_gpu_submit", render_start);
        self.viewport.did_present();
        self.pending = false;
        self.frames += 1;
        let publish_start = self.metric_start();
        self.publish(0);
        self.metric_end("draw_publish", publish_start);
        Ok(())
    }
    /// Publish changed owned records to the peer, at most once per publication
    /// window unless forced. Returns false after a failed publication, which has
    /// already been reported.
    fn publish_records(&self, _id: u64, force: bool) -> bool {
        let Some(peer) = self.peer.filter(|_| !self.peer_suspended) else {
            return true;
        };
        let changed: Vec<_> = self
            .owned_records()
            .into_iter()
            .filter(|record| {
                self.published_records.borrow().changed(
                    peer,
                    &record.app,
                    &record.store,
                    &record.checkout.id,
                )
            })
            .collect();
        if changed.is_empty() {
            self.publication.borrow_mut().clear_due();
            return true;
        }
        let now = ui::clock::Instant::now();
        let decision = if force {
            self.publication.borrow_mut().mark_published(now);
            crate::admitted_records::PublicationDecision::Now
        } else {
            self.publication.borrow_mut().decide(now)
        };
        if let crate::admitted_records::PublicationDecision::Deferred(_) = decision {
            if self.metric_start().is_some() {
                self.metrics.borrow_mut().count("publish_deferred");
            }
            return true;
        }
        for record in changed {
            let id = if record.checkout.id.is_package_primary() {
                ""
            } else {
                record.checkout.id.as_str()
            };
            let published = self.published_records.borrow_mut().publish(
                peer,
                &record.app,
                &record.store,
                &record.checkout.id,
                |document| crate::peer::local_owned_document(peer, &record.app, id, document),
            );
            if let Err(error) = published {
                crate::peer::mark_failure(peer, error.clone());
                emit(
                    &self.status,
                    serde_json::json!({"phase":"failed","error":error}),
                );
                return false;
            }
        }
        true
    }
    fn publish(&self, id: u64) {
        self.publish_with(id, false);
    }
    /// `force` publishes deferred edits now (close, hide, suspension, focus loss, IME end).
    fn publish_with(&self, id: u64, force: bool) {
        let records_start = self.metric_start();
        if !self.publish_records(id, force) {
            return;
        }
        self.metric_end("publish_records", records_start);
        let output_start = self.metric_start();
        let f = self.frame.frame_output();
        self.metric_end("publish_frame_output", output_start);
        let status_start = self.metric_start();
        let heads = self
            .record
            .as_ref()
            .and_then(|record| {
                record.store.read(&record.checkout.id, |doc| {
                    doc.base_heads()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        let document = self
            .record
            .as_ref()
            .and_then(|record| {
                record.store.read(&record.checkout.id, |doc| {
                    automerge::AutoSerde::from(doc)
                        .serialize(serde_json::value::Serializer)
                        .ok()
                })
            })
            .flatten();
        let setup_errors = self
            .desktop
            .as_ref()
            .map(|desktop| desktop.errors.borrow().clone())
            .unwrap_or_default();
        // The frame retains the original close reports and their retry owners.
        // Retry clones inspect current durability without discarding that custody.
        let component_close_errors=self.frame.component_close_reports().iter().filter_map(|report|{
            let mut report=report.clone();let _=report.retry_record();let _=report.retry_view();
            (report.record_error.is_some()||report.view_error.is_some()||report.teardown_error.is_some()).then(||serde_json::json!({"record":report.record_error,"view":report.view_error,"teardown":report.teardown_error}))
        }).collect::<Vec<_>>();
        let pending_intents = self
            .desktop
            .as_ref()
            .map(|desktop| desktop.session_requests.borrow().len())
            .unwrap_or(0);
        let ime=f.ime.map(|i|serde_json::json!({"x":i.x,"y":i.y,"width":i.width,"height":i.height,"purpose":format!("{:?}",i.purpose)}));
        // The semantics tree is the bulk of every status; the host keeps the
        // last tree it received, so an unchanged tree is left out entirely and
        // content-only changes while typing are throttled (see semantics_status).
        let decision = self.semantics.borrow_mut().decide(
            &f.semantics,
            f.ime.as_ref(),
            ui::clock::Instant::now(),
        );
        let value = serde_json::json!({"phase":if self.error.is_some(){"failed"}else if self.peer_suspended{"suspended"}else{"presented"},"inputSuspended":self.peer_suspended,"id":id,"error":self.error,"frames":self.frames,"gpuGeneration":self.generation,"logicalSize":f.logical_size,"deviceSize":f.device_size,"dpr":self.viewport.applied().dpr,"surfaceSize":self.gpu.as_ref().map(|g|[g.surface.config.width,g.surface.config.height]),"scenePaths":f.scene.encoding().n_paths,"sceneSegments":f.scene.encoding().n_path_segments,"sceneDrawTags":f.scene.encoding().draw_tags.len(),"rootRect":f.semantics.root.rect,"record":self.record.as_ref().map(|record|record.checkout.id.as_str()),"actorId":self.record.as_ref().and_then(|record|record.store.read(&record.checkout.id,|doc|doc.get_actor().to_hex_string())),"desktopErrors":setup_errors,"componentCloseErrors":component_close_errors,"pendingSessionIntents":pending_intents,"sessionIntentStatus":if pending_intents>0{Some("awaiting-native-command-admission")}else{None},"heads":heads,"document":document,"ime":ime});
        let mut text = value.to_string();
        use crate::semantics_status::SemanticsDecision;
        match decision {
            SemanticsDecision::Deliver { forced } => {
                // Splice the already serialized tree into the status object
                // instead of parsing it back into a Value and serializing twice.
                let gate = self.semantics.borrow();
                text.pop();
                text.push_str(",\"semantics\":");
                text.push_str(std::str::from_utf8(gate.text()).unwrap_or("null"));
                text.push('}');
                if forced && self.metric_start().is_some() {
                    self.metrics.borrow_mut().count("semantics_forced");
                }
            }
            SemanticsDecision::Deferred(_) => {
                if self.metric_start().is_some() {
                    self.metrics.borrow_mut().count("semantics_throttled");
                }
            }
            SemanticsDecision::Unchanged => {
                if self.metric_start().is_some() {
                    self.metrics.borrow_mut().count("semantics_unchanged");
                }
            }
        }
        emit_text(&self.status, &text);
        self.metric_end("publish_status", status_start);
    }
}
use automerge::transaction::Transactable;
use serde::Serialize;
#[wasm_bindgen]
pub fn ime(id: u64, kind: &str, text: &str) -> Result<(), JsValue> {
    let event = match kind {
        "start" => winit::event::Ime::Enabled,
        "preedit" => winit::event::Ime::Preedit(text.into(), Some((text.len(), text.len()))),
        "commit" => winit::event::Ime::Commit(text.into()),
        "end" => winit::event::Ime::Disabled,
        _ => return Err(error("invalid IME event")),
    };
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        if m.peer_suspended {
            return Ok(());
        }
        let start = m.metric_start();
        m.frame.send_host_event(&WindowEvent::Ime(event));
        m.metric_end("input_ime", start);
        if kind == "end" {
            m.publish_records(id, true);
        }
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}
#[wasm_bindgen]
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
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        if m.peer_suspended {
            return Ok(false);
        }
        let desktop = m.desktop.clone();
        crate::clipboard::during_input(&m.status, || match desktop {
            Some(desktop) => {
                desktop.perform_semantic_input(&mut m.frame, path, target.as_deref(), &action)
            }
            None => m
                .frame
                .perform_semantic_named(path, target.as_deref(), &action),
        })
        .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(true)
    })
}
/// The host needs the complete semantics tree now, for example when assistive
/// technology focus enters the surface: the next status carries it even if
/// the tree is unchanged or a content-only change is still inside its window.
#[wasm_bindgen]
pub fn request_semantics(id: u64) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        m.semantics.borrow_mut().request();
        m.publish(id);
        Ok(())
    })
}
pub(crate) fn write_json(w: &PendingWrite) -> serde_json::Value {
    serde_json::json!({"key":serde_json::to_string(&DiskKey::from(&w.key)).unwrap_or_default(),"bytes":w.value.bytes,"actor":w.value.actor,"revision":crate::revision_wire_value(w.revision)})
}
/// Copy binary persistence payloads directly into JS-owned buffers. A view into
/// WASM memory could be invalidated while IndexedDB awaits its transaction.
pub(crate) fn write_bytes(w: &PendingWrite) -> Result<JsValue, JsValue> {
    let value = js_sys::Object::new();
    js_sys::Reflect::set(
        &value,
        &"key".into(),
        &serde_json::to_string(&DiskKey::from(&w.key))
            .map_err(error)?
            .into(),
    )?;
    js_sys::Reflect::set(
        &value,
        &"bytes".into(),
        &js_sys::Uint8Array::from(w.value.bytes.as_slice()),
    )?;
    let actor = w
        .value
        .actor
        .as_ref()
        .map(|bytes| JsValue::from(js_sys::Uint8Array::from(bytes.as_slice())))
        .unwrap_or(JsValue::NULL);
    js_sys::Reflect::set(&value, &"actor".into(), &actor)?;
    js_sys::Reflect::set(
        &value,
        &"revision".into(),
        &crate::revision_wire_value(w.revision).into(),
    )?;
    Ok(value.into())
}
#[wasm_bindgen]
pub fn pending_writes(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let start = m.metric_start();
        for record in m.owned_records() {
            let _ = record.store.try_flush();
        }
        let writes = m.storage.pending_writes();
        for w in &writes {
            m.writes.insert(w.revision, w.clone());
        }
        let result = serde_json::to_string(&writes.iter().map(write_json).collect::<Vec<_>>())
            .map_err(error);
        m.metric_end("persistence", start);
        result
    })
}
#[wasm_bindgen]
pub fn acknowledge(id: u64, revision: u64, failure: Option<String>) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let write = m
            .writes
            .remove(&revision)
            .ok_or_else(|| error("unknown write revision"))?;
        m.storage
            .acknowledge(&write, failure.map_or(Ok(()), Err))
            .map_err(error)
    })
}
#[wasm_bindgen]
pub fn recover(id: u64) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        unconfigure(&m.canvas);
        m.gpu = None;
        m.generation += 1;
        m.error = None;
        initialize_gpu(id, m.generation, m.window.clone());
        Ok(())
    })
}
#[wasm_bindgen]
pub fn destroy_device(id: u64) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        if let Some(g) = &m.gpu {
            g.cx.devices[g.surface.dev_id].device.destroy();
        }
        m.error = Some("WebGPU device deliberately destroyed; recover is required".into());
        m.publish(id);
        Ok(())
    })
}

#[wasm_bindgen]
pub fn acknowledge_storage(
    profile: &str,
    device: &str,
    revision: u64,
    failure: Option<String>,
) -> Result<(), JsValue> {
    CACHES.with_borrow(|c| {
        let cache = c
            .get(&(profile.into(), device.into()))
            .ok_or_else(|| error("unknown storage namespace"))?;
        if let Some(write) = cache
            .pending_writes()
            .into_iter()
            .find(|w| w.revision == revision)
        {
            cache
                .acknowledge(&write, failure.map_or(Ok(()), Err))
                .map_err(error)?;
        }
        Ok(())
    })
}

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
        MOUNTS.with_borrow_mut(|m| {
            let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
            if m.peer_suspended {
                return Ok(false);
            }
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
            // A failed clipboard sink must never let a legacy cut delete the selection.
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

/// Read actual rendered texture pixels to separate the compute pass from canvas presentation.
#[wasm_bindgen]
pub async fn probe_pixels(id: u64) -> Result<String, JsValue> {
    let buffer = MOUNTS.with_borrow(|mounts| {
        let m = mounts.get(&id).ok_or_else(|| error("closed mount"))?;
        let gpu = m.gpu.as_ref().ok_or_else(|| error("GPU pending"))?;
        let device = &gpu.cx.devices[gpu.surface.dev_id].device;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("qos proof pixels"),
            size: 512,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for (row, (x, y)) in [
            (10, 10),
            (gpu.surface.config.width / 2, gpu.surface.config.height / 2),
        ]
        .into_iter()
        .enumerate()
        {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.surface.target_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: (row * 256) as u64,
                        bytes_per_row: Some(256),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        gpu.cx.devices[gpu.surface.dev_id]
            .queue
            .submit([encoder.finish()]);
        Ok::<_, JsValue>(buffer)
    })?;
    let (send, receive) = futures::channel::oneshot::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    receive.await.map_err(error)?.map_err(error)?;
    let bytes = buffer.slice(..).get_mapped_range();
    let value = serde_json::json!({"topLeft":&bytes[0..4],"paperCenter":&bytes[256..260]});
    drop(bytes);
    buffer.unmap();
    Ok(value.to_string())
}

#[wasm_bindgen]
pub fn font_demands(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        serde_json::to_string(&mount.frame.take_browser_font_demands()).map_err(error)
    })
}

#[wasm_bindgen]
pub fn install_font(id: u64, name: &str, bytes: Vec<u8>) -> Result<bool, JsValue> {
    let (changed, wake) = MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let changed = mount
            .frame
            .install_browser_font(name, bytes)
            .map_err(error)?;
        let wake = changed && crate::font_install_should_wake(mount.peer_suspended, mount.hidden);
        if wake {
            mount.pending = true;
            mount.window.request_redraw();
        }
        Ok::<_, JsValue>((changed, wake))
    })?;
    if wake {
        proxy()?.send_event(id).map_err(error)?;
    }
    Ok(changed)
}

#[wasm_bindgen]
pub fn font_failed(id: u64) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        mount.frame.browser_font_failed();
        Ok(())
    })
}

#[wasm_bindgen]
pub fn asset_requests(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        m.collect_desktop();
        let requests = m
            .images
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
#[wasm_bindgen]
pub fn complete_asset(
    id: u64,
    loader: usize,
    url: &str,
    bytes: Option<Vec<u8>>,
) -> Result<bool, JsValue> {
    if let (Some(hash), Some(bytes)) = (url.strip_prefix("qos-blob://"), bytes.as_ref()) {
        qdoc::blobstore::store()
            .put_checked(hash, bytes)
            .map_err(error)?;
    }
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let loader = m
            .images
            .get_mut(loader)
            .ok_or_else(|| error("unknown image loader"))?;
        let complete = loader.complete(url, bytes.as_deref()) && loader.loaded(url);
        m.pending = true;
        m.window.request_redraw();
        Ok(complete)
    })
}

#[wasm_bindgen]
pub fn retry_asset(id: u64, loader: usize, url: &str) -> Result<bool, JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let loader = mount
            .images
            .get_mut(loader)
            .ok_or_else(|| error("unknown image loader"))?;
        Ok(loader.retry(url))
    })
}

/// Publish the DOM event's modifier snapshot before Winit dispatches its key.
/// Winit web reports ModifiersChanged after KeyboardInput in the pinned fork.
#[wasm_bindgen]
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
    MOUNTS.with_borrow_mut(|mounts| {
        let m = mounts.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        if m.peer_suspended {
            return Ok(());
        }
        m.frame.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::from(flags),
        ));
        Ok(())
    })
}
#[wasm_bindgen]
pub fn visible(id: u64, visible: bool) {
    MOUNTS.with_borrow_mut(|mounts| {
        if let Some(m) = mounts.get_mut(&id) {
            if !visible {
                m.publish_records(id, true);
            }
            m.hidden = !visible;
            if visible {
                m.pending = true;
                m.window.request_redraw();
            }
        }
    });
}

/// A newly acquired cross-tab lease must hydrate durable bytes afresh. A lease
/// retained for a failed close never reaches this boundary.
#[wasm_bindgen]
pub fn refresh_storage_lease(profile: &str, device: &str) -> Result<(), JsValue> {
    CACHES.with_borrow_mut(|caches| {
        let key = (profile.to_owned(), device.to_owned());
        if let Some(cache) = caches.get(&key) {
            if !cache.pending_writes().is_empty() {
                return Err(error(
                    "unsaved actor state retains custody; retry recovery first",
                ));
            }
            if MOUNTS.with_borrow(|mounts| {
                mounts
                    .values()
                    .any(|m| m.storage.namespace() == cache.namespace())
            }) {
                return Err(error("cannot refresh an active storage lease"));
            }
        }
        caches.remove(&key);
        Ok(())
    })
}

/// Retry component-level owners first; retries may enqueue additional durable bytes.
#[wasm_bindgen]
pub fn prepare_close(id: u64) -> Result<String, JsValue> {
    let (writes, errors) = CLOSED.with_borrow_mut(|closed| closed.prepare(id));
    serde_json::to_string(&serde_json::json!({"writes":writes.iter().map(write_json).collect::<Vec<_>>(),"errors":errors})).map_err(error)
}
/// Only release captured view envelopes after the host acknowledged durable writes.
#[wasm_bindgen]
pub fn finish_close(id: u64) -> Result<(), JsValue> {
    CLOSED
        .with_borrow_mut(|closed| closed.finish(id))
        .map_err(error)
}
#[wasm_bindgen]
pub fn component_close_count() -> usize {
    CLOSED.with_borrow(|closed| closed.len())
}

pub(crate) fn load_cache(
    profile: &str,
    device: &str,
    hydrated: &JsValue,
) -> Result<PortableStorage, JsValue> {
    let storage = CACHES.with_borrow_mut(|c| {
        let key = (profile.to_owned(), device.to_owned());
        if let Some(cache) = c.get(&key) {
            return Ok(cache.clone());
        }
        let cache = PortableStorage::new(&profile, &device).map_err(error)?;
        c.insert(key, cache.clone());
        Ok::<_, JsValue>(cache)
    })?;
    for entry in js_sys::Array::from(hydrated).iter() {
        let key = Reflect::get(&entry, &"key".into())?.as_string();
        let bytes = Uint8Array::new(&Reflect::get(&entry, &"bytes".into())?).to_vec();
        let actor = Reflect::get(&entry, &"actor".into())?;
        crate::storage_key::hydrate_entry(
            &storage,
            key.as_deref(),
            bytes,
            (!actor.is_null() && !actor.is_undefined()).then(|| Uint8Array::new(&actor).to_vec()),
        )
        .map_err(error)?;
    }
    Ok(storage)
}

pub(crate) fn refresh_peer_mounts(peer: u32) {
    let state = crate::peer::desktop_state(peer);
    let permissions = crate::peer::permissions(peer);
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut().filter(|mount| mount.peer == Some(peer)) {
            if let Some(desktop) = &mount.desktop {
                // A retained document is not a continuing read grant. Withdraw history
                // until a successful admission is reconciled, including record-level loss.
                if state.is_err() || permissions.is_err() {
                    desktop.invalidate_history();
                }
                if let Ok(permissions) = &permissions {
                    *desktop.permissions.borrow_mut() = permissions.clone();
                }
                match (&state, &permissions) {
                    (Ok(state), Ok(permissions)) => {
                        if let Err(error) = desktop.admit(state, permissions.clone()) {
                            desktop.invalidate_history();
                            desktop.errors.borrow_mut().push(error);
                        } else {
                            mount.peer_suspended = false;
                        }
                    }
                    (Err(reason), _)
                        if reason.contains("forbidden") || reason.contains("revoked") =>
                    {
                        mount.peer_suspended = true;
                        desktop.errors.borrow_mut().push(reason.clone());
                    }
                    (_, Err(reason)) => {
                        mount.peer_suspended = true;
                        desktop.errors.borrow_mut().push(reason.clone());
                    }
                    _ => {}
                }
            }
            // A presenting mount applies the queued admission in its next frame
            // (the admission inbox marks the desktop for redraw, and the draw
            // path collects desktop/search state around its relayout). An
            // immediate relayout here ran the whole update/layout/draw pipeline
            // inside every delivery callback, including each echo of this
            // peer's own edits, and the next frame repeated it. Mounts that
            // cannot draw now (hidden, suspended, failed, no GPU yet) keep the
            // immediate application so their component state stays current.
            let presents = !mount.hidden
                && !mount.peer_suspended
                && mount.error.is_none()
                && mount.gpu.is_some()
                && !mount.viewport.suspended();
            if !presents {
                mount.frame.relayout();
                mount.collect_desktop();
            }
            mount.pending = true;
            mount.window.request_redraw();
        }
    });
}

#[wasm_bindgen]
pub fn rebind_peer(old: u32, new: u32) {
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut().filter(|mount| mount.peer == Some(old)) {
            mount.peer = Some(new);
            mount.peer_suspended = false;
            mount.publication.borrow_mut().reset();
            mount.queue_resize();
            mount.frame.relayout();
            mount.pending = true;
            mount.window.request_redraw();
        }
    });
}

#[wasm_bindgen]
pub fn suspend_peer(peer: u32, suspended: bool) -> Result<(), JsValue> {
    if suspended {
        prepare_peer_remote(peer).map_err(error)?;
    }
    MOUNTS.with_borrow_mut(|mounts|for mount in mounts.values_mut().filter(|mount|mount.peer==Some(peer)){mount.peer_suspended=suspended;emit(&mount.status,serde_json::json!({"phase":if suspended{"suspended"}else{"presented"},"inputSuspended":suspended}));});
    Ok(())
}

pub(crate) fn prepare_peer_remote(peer: u32) -> Result<(), String> {
    MOUNTS.with_borrow(|mounts| {
        for mount in mounts.values().filter(|mount| mount.peer == Some(peer)) {
            for record in mount.owned_records() {
                let document = crate::admitted_records::prepare_remote_owned(
                    &record.store,
                    &record.checkout.id,
                )?;
                let id = if record.checkout.id.is_package_primary() {
                    ""
                } else {
                    record.checkout.id.as_str()
                };
                crate::peer::local_owned_document(peer, &record.app, id, document)?;
            }
        }
        Ok(())
    })
}

/// Caller projection is supplied only by the session's durable private client.
#[wasm_bindgen]
pub fn caller_actions(id: u64, json: &str) -> Result<(), JsValue> {
    let resources = crate::action_palette::resources(json).map_err(error)?;
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("action surface disposed"))?;
        mount.frame.set_caller_actions(resources);
        mount.pending = true;
        mount.window.request_redraw();
        Ok(())
    })
}

#[wasm_bindgen]
pub fn take_caller_action(id: u64) -> Result<Option<String>, JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("action surface disposed"))?;
        if mount.peer_suspended {
            return Ok(None);
        }
        mount
            .frame
            .take_caller_operation()
            .map(|pending| {
                crate::action_palette::request(pending)
                    .map(|value| value.to_string())
                    .map_err(error)
            })
            .transpose()
    })
}

/// IDs stay inside an opaque JSON string until native Rust decodes the command.
#[wasm_bindgen]
pub fn pending_desktop_command(id: u64) -> Result<Option<String>, JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let mount = mounts.get(&id).ok_or_else(|| error("desktop disposed"))?;
        if mount.peer_suspended { return Ok(None); }
        mount.desktop.as_ref().and_then(|desktop| desktop.session_requests.borrow().first().cloned().map(|request| (desktop, request)))
            .map(|(desktop, (invocation, command))| {
                let mut value = serde_json::json!({
                    "invocation": invocation, "command": serde_json::to_string(&command).map_err(error)?,
                });
                // Optimistic reorder: where the group lands, measured before the
                // local move, for the remote members' live ghost.
                if let Some(preview) = desktop.speculation.borrow().preview(invocation) {
                    value["preview"] = serde_json::json!({"group": preview.group.to_string(), "dx": preview.dx, "width": preview.width, "height": preview.height});
                }
                serde_json::to_string(&value).map_err(error)
            }).transpose()
    })
}

/// The host's command bridge reports terminal outcomes, so queued pure
/// reorders may render optimistically (see `desktop_host/speculation.rs`).
#[wasm_bindgen]
pub fn desktop_speculation_enable(id: u64, enabled: bool) -> Result<(), JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let mount = mounts.get(&id).ok_or_else(|| error("desktop disposed"))?;
        if let Some(desktop) = &mount.desktop {
            desktop.speculation.borrow_mut().enabled = enabled;
        }
        Ok(())
    })
}

/// Terminal outcome of a queued desktop command (its Rust invocation): a
/// failure snaps an optimistic reorder back to durable state; a success lets
/// it retire once durable state shows it. `None` only schedules a frame that
/// re-checks the confirmation hold and deadlines.
#[wasm_bindgen]
pub fn desktop_command_settled(
    id: u64,
    invocation: &str,
    succeeded: Option<bool>,
) -> Result<bool, JsValue> {
    let invocation: uuid::Uuid = invocation.parse().map_err(error)?;
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("desktop disposed"))?;
        let Some(desktop) = &mount.desktop else {
            return Ok(false);
        };
        let mut speculation = desktop.speculation.borrow_mut();
        if speculation.is_empty() {
            return Ok(false);
        }
        let changed = match succeeded {
            Some(succeeded) => speculation.settle(invocation, succeeded, web_time::Instant::now()),
            None => true,
        };
        drop(speculation);
        if changed {
            mount.pending = true;
            mount.window.request_redraw();
        }
        Ok(changed)
    })
}

#[wasm_bindgen]
pub fn acknowledge_desktop_command(id: u64, invocation: &str) -> Result<(), JsValue> {
    let invocation: uuid::Uuid = invocation.parse().map_err(error)?;
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("desktop disposed"))?;
        if let Some(desktop) = &mount.desktop {
            let mut pending = desktop.session_requests.borrow_mut();
            if pending.first().is_some_and(|(id, _)| *id == invocation) {
                pending.remove(0);
            }
        }
        mount.pending = true;
        mount.window.request_redraw();
        Ok(())
    })
}

#[wasm_bindgen]
pub fn desktop_navigation_snapshot(id: u64) -> Result<Option<String>, JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let mount = mounts.get(&id).ok_or_else(|| error("Desktop disposed"))?;
        Ok(mount
            .desktop
            .as_ref()
            .map(|d| d.navigation.borrow().to_string()))
    })
}
#[wasm_bindgen]
pub fn desktop_navigation_revision(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let mount = mounts.get(&id).ok_or_else(|| error("Desktop disposed"))?;
        Ok(mount
            .desktop
            .as_ref()
            .map(|d| d.navigation_revision.get())
            .unwrap_or(0)
            .to_string())
    })
}
#[wasm_bindgen]
pub fn desktop_input_ownership(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let mount = mounts.get(&id).ok_or_else(|| error("Desktop disposed"))?;
        let desktop = mount.desktop.as_ref().ok_or_else(|| error("Desktop navigation unavailable"))?;
        Ok(serde_json::json!({
            "sequence": mount.frame.input_ownership_sequence().to_string(),
            "activation": desktop.activation_sequence.get().to_string(),
            "owner": mount.frame.inspection().keyboard_focus_owner,
            "ime": mount.frame.ime_window().map(|i| serde_json::json!({"x":i.x,"y":i.y,"width":i.width,"height":i.height,"purpose":format!("{:?}",i.purpose)})),
        }).to_string())
    })
}

#[wasm_bindgen]
pub fn desktop_navigate(id: u64, command: &str) -> Result<Option<String>, JsValue> {
    let command = serde_json::from_str(command).map_err(error)?;
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("Desktop disposed"))?;
        if mount.peer_suspended {
            return Err(error("Desktop unavailable while peer is suspended"));
        }
        let desktop = mount
            .desktop
            .clone()
            .ok_or_else(|| error("Desktop navigation unavailable"))?;
        let invocation = desktop.navigate(&mut mount.frame, command).map_err(error)?;
        mount.pending = true;
        mount.window.request_redraw();
        Ok(invocation)
    })
}

#[wasm_bindgen]
pub fn desktop_command_search(id: u64, generation: &str, query: &str) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("Desktop disposed"))?;
        if mount.peer_suspended {
            return Err(error("Desktop suspended"));
        }
        let desktop = mount
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop search unavailable"))?;
        if desktop.navigation.borrow()["generation"].as_str() != Some(generation) {
            return Err(error("Stale desktop surface"));
        }
        if query.len() > 512 {
            return Err(error("Search is too long"));
        }
        mount.frame.open_caller_search(query).map_err(error)?;
        mount.pending = true;
        mount.window.request_redraw();
        Ok(())
    })
}

/// Full system search; the caller-only commands opener remains separate.
#[wasm_bindgen]
pub fn desktop_global_search(id: u64, generation: &str, query: &str) -> Result<(), JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let mount = mounts
            .get_mut(&id)
            .ok_or_else(|| error("Desktop disposed"))?;
        if mount.peer_suspended {
            return Err(error("Desktop suspended"));
        }
        let desktop = mount
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop search unavailable"))?;
        desktop
            .open_global_search(&mut mount.frame, generation, query)
            .map_err(error)?;
        mount.pending = true;
        mount.window.request_redraw();
        Ok(())
    })
}

/// Opt-in bounded diagnostic capture; disabled by default and never persisted.
#[wasm_bindgen]
pub fn configure_frame_metrics(enabled: bool) {
    MOUNTS.with_borrow_mut(|mounts| {
        for mount in mounts.values_mut() {
            mount.metrics.borrow_mut().configure(enabled);
            mount.frame.set_pointer_redraw_diagnostics(enabled);
            // Per-pass layout timing (sync, build/compute, apply, draw, input) for
            // every frame the harness renders while metrics are on.
            if enabled {
                mount.frame.begin_profiling();
            } else {
                mount.frame.end_profiling();
            }
        }
    });
}
#[wasm_bindgen]
pub fn take_frame_metrics() -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|mounts| {
        let samples: Vec<_> = mounts.iter_mut().map(|(id, mount)| {
            let mut metrics = mount.metrics.borrow_mut();
            let passes = mount.frame.take_profile_samples();
            let relayout = mount.frame.last_relayout();
            serde_json::json!({"mount": id, "samples": metrics.take(), "dropped": metrics.take_dropped(), "notes": metrics.take_notes(), "redrawSources": mount.frame.redraw_sources(), "layoutPasses": passes, "lastRelayout": {"rebuilt": relayout.rebuilt, "measured": relayout.measured, "markedDirty": relayout.marked_dirty, "markedLeaves": relayout.marked_leaves}})
        }).collect();
        serde_json::to_string(&samples).map_err(error)
    })
}

thread_local! {
    /// LIVE_V1: remote carets/selections projected onto each mount's presence.
    static LIVE_REMOTE: RefCell<HashMap<u64, crate::live_view::RemoteProjection>> =
        RefCell::new(HashMap::new());
}
/// LIVE_V1 lane E: the local presence snapshot the overlay publishes and maps
/// remote pointers with (focus record, window rectangles, editor anchors,
/// window drag). `null` for a single-record mount.
#[wasm_bindgen]
pub fn live_view_snapshot(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow(|mounts| {
        let m = mounts.get(&id).ok_or_else(|| error("unknown mount"))?;
        let Some(desktop) = &m.desktop else {
            return Ok("null".into());
        };
        let selection = m.frame.focus_presence_payload();
        let layout = desktop.live.borrow();
        let records = desktop.tab_records.borrow();
        Ok(crate::live_view::snapshot_json(&layout, &records, selection).to_string())
    })
}
/// LIVE_V1 lane E: replace the remote carets/selections drawn by the
/// presence-aware editors. Only a change schedules a redraw (no relayout).
#[wasm_bindgen]
pub fn live_view_apply(id: u64, remotes: &str) -> Result<bool, JsValue> {
    let inputs: Vec<crate::live_view::RemoteInput> =
        serde_json::from_str(remotes).map_err(error)?;
    MOUNTS.with_borrow_mut(|mounts| {
        LIVE_REMOTE.with_borrow_mut(|all| all.retain(|mount, _| mounts.contains_key(mount)));
        let m = mounts.get_mut(&id).ok_or_else(|| error("unknown mount"))?;
        let changed = LIVE_REMOTE.with_borrow_mut(|all| {
            let projection = all.entry(id).or_default();
            m.frame
                .update_presence(|presence| projection.apply(presence, inputs))
        });
        if changed && !m.peer_suspended {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(changed)
    })
}
