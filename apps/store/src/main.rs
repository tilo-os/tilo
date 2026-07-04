//! Binary shim for the qOS `store` — mirrors the sibling native apps.
//!
//! Two modes:
//!
//!   * default — build a [`store::StoreApp`], hand it to `ui::App`, run
//!     the live event loop. On startup we spawn a small worker thread
//!     that refreshes the item list from configured registries + Flathub
//!     and streams snapshots back via an `mpsc` channel; the app widget
//!     drains that channel each frame from inside `sync`.
//!   * `--shots <PATH>` — render one offscreen frame to `<PATH>` and exit.

use std::sync::mpsc;
use std::time::Duration;

use store::config::{load_registries, RegistryConfig};
use store::sources::{flathub_placeholder_to_item, merge, qos_to_store_item};
use store::{StoreApp, StoreItem};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_flag(&args, "--shots") {
        let viewport = parse_flag(&args, "--viewport")
            .and_then(|v| store::shots::parse_viewport(&v))
            .unwrap_or(store::shots::DEFAULT_SHOTS_VIEWPORT);
        store::shots::render_to_path(viewport, std::path::Path::new(&out))?;
        return Ok(());
    }

    // Channel is unbounded because refreshes are minutes-apart —
    // there's no backpressure to worry about.
    let (tx, rx) = mpsc::channel::<Snapshot>();
    spawn_refresh_worker(tx);

    let widget = LiveStore::new(rx);
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
    app.run();
    Ok(())
}

/// One refresh snapshot the worker sends over the wire.
struct Snapshot {
    items: Vec<StoreItem>,
    status_line: String,
}

/// The Widget the main event loop owns. Wraps a `StoreApp` and
/// drains snapshots from the worker inside `sync`.
struct LiveStore {
    inner: StoreApp,
    rx: mpsc::Receiver<Snapshot>,
}

impl LiveStore {
    fn new(rx: mpsc::Receiver<Snapshot>) -> Self {
        Self {
            inner: StoreApp::new(),
            rx,
        }
    }
}

impl ui::widget::Widget for LiveStore {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        // Drain any snapshots the worker produced since last frame.
        while let Ok(snap) = self.rx.try_recv() {
            self.inner.set_items(snap.items);
            self.inner.set_status_line(snap.status_line);
        }
        self.inner.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }
    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.inner.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text_renderer, theme, presence);
    }
    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.inner
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }
    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        self.inner.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
        self.inner.children_mut()
    }
    fn name(&self) -> &str {
        "LiveStore"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }
    fn needs_animation_frame(&self) -> bool {
        self.inner.needs_animation_frame()
    }
}

/// Spawn the background refresh thread. The initial snapshot is
/// pushed immediately so first-render shows *something* real (or an
/// empty state) rather than a blank surface while the network
/// round-trip finishes.
fn spawn_refresh_worker(tx: mpsc::Sender<Snapshot>) {
    std::thread::spawn(move || loop {
        let items = collect_items();
        let status = if items.is_empty() {
            "No apps yet. Check your registries.toml + network.".to_string()
        } else {
            format!("Loaded {} apps.", items.len())
        };
        if tx.send(Snapshot { items, status_line: status }).is_err() {
            // UI thread is gone; wind down.
            break;
        }
        // Refresh every 15 minutes.
        std::thread::sleep(Duration::from_secs(15 * 60));
    });
}

/// Fetch and merge — pure sequential; runs off the UI thread.
fn collect_items() -> Vec<StoreItem> {
    let registries = load_registries().registries;
    let installed_qos = store::installed::load_installed();
    let installed_flat = store::flathub_client::installed_flatpaks();

    let mut qos_items: Vec<StoreItem> = Vec::new();
    for reg in registries.iter().filter(|r| r.enabled) {
        match store::registry_client::fetch_entries(reg) {
            Ok(entries) => {
                for v in entries {
                    qos_items.push(qos_to_store_item(&v, &reg.name, &installed_qos));
                }
            }
            Err(e) => {
                tracing::warn!("registry {} refresh failed: {e}", reg.name);
            }
        }
    }

    let flathub_items = match store::flathub_client::fetch_popular(200) {
        Ok(popular) => popular
            .iter()
            .map(|p| flathub_placeholder_to_item(p, &installed_flat))
            .collect(),
        Err(e) => {
            tracing::warn!("flathub refresh failed: {e}");
            Vec::new()
        }
    };

    let _ = registries_summary(&registries);
    merge(qos_items, flathub_items)
}

fn registries_summary(registries: &[RegistryConfig]) -> String {
    let enabled = registries.iter().filter(|r| r.enabled).count();
    format!(
        "configured registries: {} total / {} enabled",
        registries.len(),
        enabled
    )
}

/// Read `--name value` or `--name=value` out of `args`.
fn parse_flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == name {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}
