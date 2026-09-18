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
    portable_storage::{PendingWrite, PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners, RecordStore},
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
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum DiskKey {
    Admission { workspace: String },
    Record {
        package: String,
        workspace: String,
        record: String,
    },
    Primary {
        package: String,
        workspace: String,
    },
    View {
        workspace: String,
        instance: u64,
    },
}
impl From<&StorageKey> for DiskKey {
    fn from(k: &StorageKey) -> Self {
        match k {
            StorageKey::Admission { workspace } => Self::Admission { workspace:workspace.clone() },
            StorageKey::Record {
                package,
                workspace,
                record,
            } => Self::Record {
                package: package.clone(),
                workspace: workspace.clone(),
                record: record.clone(),
            },
            StorageKey::Primary { package, workspace } => Self::Primary {
                package: package.clone(),
                workspace: workspace.clone(),
            },
            StorageKey::View {
                workspace,
                instance,
            } => Self::View {
                workspace: workspace.clone(),
                instance: *instance,
            },
        }
    }
}
impl From<DiskKey> for StorageKey {
    fn from(k: DiskKey) -> Self {
        match k {
            DiskKey::Admission { workspace } => Self::Admission { workspace },
            DiskKey::Record {
                package,
                workspace,
                record,
            } => Self::Record {
                package,
                workspace,
                record,
            },
            DiskKey::Primary { package, workspace } => Self::Primary { package, workspace },
            DiskKey::View {
                workspace,
                instance,
            } => Self::View {
                workspace,
                instance,
            },
        }
    }
}
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
    peer_suspended: bool,
    peer:Option<u32>,
    app_id:String,
    window: Arc<Window>,
    canvas: web_sys::HtmlCanvasElement,
    frame: FrameHarness,
    storage: PortableStorage,
    records: RecordStore,
    checkout: RecordCheckout,
    gpu: Option<Gpu>,
    pending: bool,
    hidden: bool,
    generation: u64,
    frames: u64,
    status: Function,
    writes: HashMap<u64, PendingWrite>,
    images: Vec<ui::image_store::HostImageLoader>,
    error: Option<String>,
}
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn emit(f: &Function, v: serde_json::Value) {
    let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&v.to_string()));
}
fn proxy() -> Result<EventLoopProxy<u64>, JsValue> {
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
        mount.publish(id);
        mount.frame.close_components();
        let reports=mount.frame.take_component_close_reports();
        let _=mount.records.try_flush();
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
                    if m.peer_suspended {return;}
                    if let Err(e) = m.resize() {
                        m.error = Some(e);
                    }
                    m.window.request_redraw();
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
                    if m.peer_suspended { return; }
                    m.frame.send_host_event(&event);
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
            for m in mounts.values() {
                if m.peer_suspended || m.hidden || m.error.is_some() || m.gpu.is_none() {
                    continue;
                }
                let output = m.frame.frame_output();
                if m.pending || output.needs_redraw || output.needs_animation_frame {
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
    let definition = match config.app.as_str() {
        "writer" => word_processor::definition(),
        "chat" => chat::definition(),
        _ => return Err(error("unsupported application")),
    };
    let storage=load_cache(&config.profile,&config.device,&hydrated)?;
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
            Err(ui::record::RecordOpenError::MissingRecord) if config.peer.is_none() => LaunchTarget::PrimaryNew,
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
    let permissions = Rc::new(RefCell::new(if let Some(peer)=config.peer {
        crate::peer::permissions(peer).map_err(error)?
    }else{PermissionContext::from_capabilities(Actor::person(Some(Role::Owner), &config.author),vec![])}));
    let context = HostContext::prepare(
        definition,
        records.clone(),
        views,
        config.device.clone(),
        config.instance,
        permissions,
        UserId(config.author),
        qos_actions::ActionContext::default(),
        &target,
    )
    .map_err(error)?;
    let loaders = Rc::new(RefCell::new(Vec::new()));
    let destination = loaders.clone();
    let component = ui::ImageStore::with_host_factory(
        Rc::new(move || {
            let (image, loader) = ui::ImageStore::host_driven();
            destination.borrow_mut().push(loader);
            image
        }),
        || definition.instantiate(context, target, restore),
    )
    .map_err(error)?;
    let images = std::mem::take(&mut *loaders.borrow_mut());
    let checkout = component.record().clone();
    let system = ui::font_catalog::FontCatalog::product("", "en-US")
        .load_bytes(|name| {
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
            text_renderer: ui::TextRenderer::with_font_system(system),
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
        peer_suspended: false,
        peer:config.peer,
        app_id:definition.app_id.into(),
        window: window.clone(),
        canvas,
        frame,
        storage,
        records,
        checkout,
        gpu: None,
        pending: true,
        hidden: false,
        generation: 1,
        frames: 0,
        status: status.clone(),
        writes: HashMap::new(),
        images,
        error: None,
    };
    MOUNTS.with_borrow_mut(|m| m.insert(id, mount));
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
        MOUNTS.with_borrow_mut(|mounts|{
            let Some(m)=mounts.get_mut(&id).filter(|m|m.generation==generation) else{return;};
            match result{Ok(gpu)=>{
                emit(&m.status,serde_json::json!({"initialSurfaceSize":[gpu.surface.config.width,gpu.surface.config.height]}));
                m.gpu=Some(gpu);
                if let Err(e)=m.resize(){m.error=Some(e);}
                m.pending=true;m.window.request_redraw();
            },Err(e)=>{m.error=Some(format!("WebGPU unavailable: {e}"));m.publish(id);}}
        });
    });
}
impl Mount {
    fn resize(&mut self) -> Result<(), String> {
        let size = self.window.inner_size();
        let dpr = self.window.scale_factor();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.frame.set_input_scale(dpr)?;
        self.frame
            .resize((
                (size.width as f64 / dpr).max(1.0) as u32,
                (size.height as f64 / dpr).max(1.0) as u32,
            ))
            .map_err(|e| e.to_string())?;
        if let Some(gpu) = &mut self.gpu {
            gpu.cx
                .resize_surface(&mut gpu.surface, size.width, size.height);
        }
        self.pending = true;
        Ok(())
    }
    fn draw(&mut self) -> Result<(), String> {
        if self.peer_suspended || self.hidden || self.error.is_some() {
            return Ok(());
        }
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.pending |= self.frame.poll_frame().map_err(|e| e.to_string())?;
        if !self.pending {
            return Ok(());
        }
        let Some(gpu) = &mut self.gpu else {
            return Ok(());
        };
        if gpu.surface.config.width != size.width || gpu.surface.config.height != size.height {
            gpu.cx
                .resize_surface(&mut gpu.surface, size.width, size.height);
        }
        let handle = &gpu.cx.devices[gpu.surface.dev_id];
        if let Some(reason) = handle.loss_monitor().reason() {
            return Err(format!(
                "WebGPU device lost: {reason}; call recover to recreate the renderer"
            ));
        }
        let output = match gpu.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.cx.configure_surface(&gpu.surface);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return Ok(());
            }
            other => return Err(format!("WebGPU surface unavailable: {other:?}")),
        };
        let frame = self.frame.frame_output();
        let mut physical = vello::Scene::new();
        physical.append(
            frame.scene,
            Some(ui::scale::composite_transform(self.window.scale_factor())),
        );
        gpu.renderer
            .render_to_texture(
                &handle.device,
                &handle.queue,
                &physical,
                &gpu.surface.target_view,
                &vello::RenderParams {
                    base_color: frame.background,
                    width: size.width,
                    height: size.height,
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
        self.pending = false;
        self.frames += 1;
        self.publish(0);
        Ok(())
    }
    fn publish(&self, id: u64) {
        if let Some(peer)=self.peer.filter(|_| !self.peer_suspended) {
            let captured=crate::admitted_records::capture(&self.records,&self.checkout.id);
            let record=if self.checkout.id.is_package_primary(){""}else{self.checkout.id.as_str()};
            if let Err(error)=captured.and_then(|bytes|crate::peer::local_document(peer,&self.app_id,record,bytes)){crate::peer::mark_failure(peer,error.clone());emit(&self.status,serde_json::json!({"phase":"failed","error":error}));return;}
        }
        let f = self.frame.frame_output();
        let heads = self
            .records
            .read(&self.checkout.id, |doc| {
                doc.base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let document = self
            .records
            .read(&self.checkout.id, |doc| {
                automerge::AutoSerde::from(doc)
                    .serialize(serde_json::value::Serializer)
                    .ok()
            })
            .flatten();
        let ime=f.ime.map(|i|serde_json::json!({"x":i.x,"y":i.y,"width":i.width,"height":i.height,"purpose":format!("{:?}",i.purpose)}));
        emit(
            &self.status,
            serde_json::json!({"phase":if self.error.is_some(){"failed"}else if self.peer_suspended{"suspended"}else{"presented"},"inputSuspended":self.peer_suspended,"id":id,"error":self.error,"frames":self.frames,"gpuGeneration":self.generation,"logicalSize":f.logical_size,"deviceSize":f.device_size,"dpr":self.window.scale_factor(),"surfaceSize":self.gpu.as_ref().map(|g|[g.surface.config.width,g.surface.config.height]),"scenePaths":f.scene.encoding().n_paths,"sceneSegments":f.scene.encoding().n_path_segments,"sceneDrawTags":f.scene.encoding().draw_tags.len(),"rootRect":f.semantics.root.rect,"record":self.checkout.id.as_str(),"actorId":self.records.read(&self.checkout.id,|doc|doc.get_actor().to_hex_string()),"heads":heads,"document":document,"semantics":f.semantics,"ime":ime}),
        );
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
        if m.peer_suspended { return Ok(()); }
        m.frame.send_host_event(&WindowEvent::Ime(event));
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
        if m.peer_suspended { return Ok(false); }
        m.frame
            .perform_semantic_named(path, target.as_deref(), &action)
            .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(true)
    })
}
pub(crate) fn write_json(w: &PendingWrite) -> serde_json::Value {
    serde_json::json!({"key":serde_json::to_string(&DiskKey::from(&w.key)).unwrap_or_default(),"bytes":w.value.bytes,"actor":w.value.actor,"revision":crate::revision_wire_value(w.revision)})
}
#[wasm_bindgen]
pub fn pending_writes(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
        let _ = m.records.try_flush();
        let writes = m.storage.pending_writes();
        for w in &writes {
            m.writes.insert(w.revision, w.clone());
        }
        serde_json::to_string(&writes.iter().map(write_json).collect::<Vec<_>>()).map_err(error)
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
            if m.peer_suspended { return Ok(false); }
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
pub fn asset_requests(id: u64) -> Result<String, JsValue> {
    MOUNTS.with_borrow_mut(|m| {
        let m = m.get_mut(&id).ok_or_else(|| error("closed mount"))?;
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
pub fn retry_asset(id:u64,loader:usize,url:&str)->Result<bool,JsValue>{
    MOUNTS.with_borrow_mut(|mounts|{let mount=mounts.get_mut(&id).ok_or_else(||error("closed mount"))?;let loader=mount.images.get_mut(loader).ok_or_else(||error("unknown image loader"))?;Ok(loader.retry(url))})
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
        if m.peer_suspended {return Ok(());}
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

pub(crate) fn load_cache(profile:&str,device:&str,hydrated:&JsValue)->Result<PortableStorage,JsValue>{
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
        let key: DiskKey = serde_json::from_str(
            &Reflect::get(&entry, &"key".into())?
                .as_string()
                .ok_or_else(|| error("missing storage key"))?,
        )
        .map_err(error)?;
        let key = StorageKey::from(key);
        if storage.get(&key).is_none() {
            let bytes = Uint8Array::new(&Reflect::get(&entry, &"bytes".into())?).to_vec();
            let actor = Reflect::get(&entry, &"actor".into())?;
            storage
                .hydrate(
                    key,
                    bytes,
                    (!actor.is_null() && !actor.is_undefined())
                        .then(|| Uint8Array::new(&actor).to_vec()),
                )
                .map_err(error)?;
        }
    }
    Ok(storage)
}

pub(crate) fn refresh_peer_mounts(peer:u32){
 MOUNTS.with_borrow_mut(|mounts|for mount in mounts.values_mut().filter(|mount|mount.peer==Some(peer)){
  mount.frame.relayout();mount.pending=true;mount.window.request_redraw();
 });
}

#[wasm_bindgen]
pub fn rebind_peer(old:u32,new:u32){MOUNTS.with_borrow_mut(|mounts|for mount in mounts.values_mut().filter(|mount|mount.peer==Some(old)){mount.peer=Some(new);mount.peer_suspended=false;let _=mount.resize();mount.frame.relayout();mount.pending=true;mount.window.request_redraw();});}

#[wasm_bindgen]
pub fn suspend_peer(peer:u32,suspended:bool)->Result<(),JsValue>{
 if suspended { prepare_peer_remote(peer).map_err(error)?; }
 MOUNTS.with_borrow_mut(|mounts|for mount in mounts.values_mut().filter(|mount|mount.peer==Some(peer)){mount.peer_suspended=suspended;emit(&mount.status,serde_json::json!({"phase":if suspended{"suspended"}else{"presented"},"inputSuspended":suspended}));});
 Ok(())
}

pub(crate) fn prepare_peer_remote(peer:u32)->Result<(),String>{
 MOUNTS.with_borrow(|mounts|{
  for mount in mounts.values().filter(|mount|mount.peer==Some(peer)){
   let bytes=crate::admitted_records::prepare_remote(&mount.records,&mount.checkout.id)?;
   let record=if mount.checkout.id.is_package_primary(){""}else{mount.checkout.id.as_str()};
   crate::peer::local_document(peer,&mount.app_id,record,bytes)?;
  }
  Ok(())
 })
}
