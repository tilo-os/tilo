//! A tilolang app on a web page: the package and its window side by side in the page.
//!
//! On a device the launcher starts the runtime with the package's payload (`TILO_APP_MODULE`), and
//! the window's instance runs in a tilolang host. A page has neither: it installs the package
//! from the address its payload is served at ([`install_package`], `installTilolangPackage` to
//! the page), which registers the app with the page's runtime under the package's own id, and
//! the window runs the package's instance itself ([`PageWindow`]), over a store of resources the
//! page keeps in memory, sending the app window's half of the window ([`AppWindow`]) the same
//! component patches a host sends over its socket.

// A page runs on one thread: what it awaits never moves to another.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        clippy::future_not_send,
        reason = "a page's futures run on its one thread"
    )
)]

#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use std::sync::Arc;

#[cfg(target_arch = "wasm32")]
use tilo_app_window::AppWindow;
#[cfg(target_arch = "wasm32")]
use tilo_render_protocol::patch::local;
use tilolang_tilo::bundle::Bundle;
#[cfg(target_arch = "wasm32")]
use tilolang_tilo::bundle::{IDS_FILE, IMPORTS_DIR, WASM_FILE};
use tilolang_tilo::host::Host;
#[cfg(target_arch = "wasm32")]
use tilolang_tilo::host::RootView;
#[cfg(target_arch = "wasm32")]
use tilolang_tilo::instance::{Config, Instance, Mode};
use tilolang_tilo::instance::{GlobalLocale, Locale, Platform};
#[cfg(target_arch = "wasm32")]
use ui::widget::{AppModel, DrawChild as _, ModelPath, Widget};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Install the package whose payload is served under `base` (a URL ending in `/`): its module,
/// its ids lock and the modules of the packages it uses (`imports/<app>.wasm`), as a `.tapp`'s
/// payload holds them. Registers the app with the page's runtime and answers its id, which the
/// page mounts. Its window opens with `scenarios` staged in its document, in order (names separated
/// by commas, or `all` for every scenario the package declares), as a capture stages one, and at
/// the capture's instant ([`ui::clock::freeze_for_capture`]).
///
/// # Errors
/// A file could not be fetched, the payload is not a tilolang package, or a package is already
/// installed in the page.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = installTilolangPackage)]
pub async fn install_package(base: String, scenarios: Option<String>) -> Result<String, JsValue> {
    let base = if base.ends_with('/') {
        base
    } else {
        format!("{base}/")
    };
    let wasm = fetch(&format!("{base}{WASM_FILE}")).await?.ok_or_else(|| {
        JsValue::from_str(&format!("{base}{WASM_FILE}: no tilolang module there"))
    })?;
    let ids = fetch(&format!("{base}{IDS_FILE}"))
        .await?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let imports = imports(&base, &wasm).await?;
    let bundle = Bundle::from_parts(wasm, ids, &imports).map_err(|error| error.to_string())?;
    let app = bundle.meta.app.clone();
    let staged: Vec<String> = match scenarios.as_deref().map(str::trim) {
        None | Some("") => Vec::new(),
        Some("all") => bundle
            .meta
            .scenarios
            .iter()
            .map(|scenario| scenario.name.clone())
            .collect(),
        Some(names) => names
            .split(',')
            .map(|name| name.trim().to_owned())
            .collect(),
    };
    // A window staged as a capture renders at the capture's instant, as a device's capture and
    // a scenario drive do, so the page shows the days and times those show.
    if !staged.is_empty() {
        ui::clock::freeze_for_capture();
    }
    STAGED.with(|held| *held.borrow_mut() = staged);
    crate::install(bundle).map_err(|error| error.to_string())?;
    ui::application::registry::register(crate::definition());
    Ok(app)
}

/// The modules of the packages the module `wasm` uses, and of those they use, each before those
/// that use it. A package the payload carries no module of (a native app's, whose actions run at
/// its own API) is not one.
#[cfg(target_arch = "wasm32")]
async fn imports(base: &str, wasm: &[u8]) -> Result<Vec<Vec<u8>>, JsValue> {
    let mut wanted: Vec<String> = uses(wasm);
    let mut seen: Vec<String> = Vec::new();
    let mut fetched: Vec<Vec<u8>> = Vec::new();
    while let Some(app) = wanted.pop() {
        if seen.contains(&app) {
            continue;
        }
        seen.push(app.clone());
        if let Some(module) = fetch(&format!("{base}{IMPORTS_DIR}/{app}.wasm")).await? {
            wanted.extend(uses(&module));
            fetched.push(module);
        }
    }
    // Found from the app outward: the packages used last come first.
    fetched.reverse();
    Ok(fetched)
}

#[cfg(target_arch = "wasm32")]
fn uses(wasm: &[u8]) -> Vec<String> {
    tilolang_tilo::tilolang::PackageMeta::from_wasm(wasm).map_or_else(Vec::new, |meta| meta.uses)
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// The scenarios the page asked its first window to open with.
    static STAGED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The bytes at `url`; `None` when the server has nothing there.
#[cfg(target_arch = "wasm32")]
async fn fetch(url: &str) -> Result<Option<Vec<u8>>, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let response: web_sys::Response =
        wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
            .await?
            .dyn_into()?;
    if response.status() == 404 {
        return Ok(None);
    }
    if !response.ok() {
        return Err(JsValue::from_str(&format!("{url}: {}", response.status())));
    }
    let buffer = wasm_bindgen_futures::JsFuture::from(response.array_buffer()?).await?;
    Ok(Some(js_sys::Uint8Array::new(&buffer).to_vec()))
}

/// Browser pages share the toolkit's catalogues, language and region formats on the page's one
/// thread. Because a browser page has no desktop settings watcher or on-disk language packs,
/// `words_revision` can only reflect in-page `I18nManager` or region-format changes, not an
/// external catalogue changing while the page runs.
static PAGE_LOCALE: GlobalLocale = tilo_app_window::platform_locale!(GlobalLocale);

/// What the instance asks of the page: the installed package, the platform's words, and the
/// person's clock, as the window reads them.
struct PagePlatform {
    bundle: Arc<Bundle>,
    wake: ui::wake::WakeHandle,
}

impl Platform for PagePlatform {
    fn load(&self, host: &Host) -> u64 {
        if let Err(error) = host.load_bundle_using(&self.bundle, |_| false) {
            tracing::error!(app = self.bundle.meta.app, %error, "the package did not load");
        }
        0
    }

    fn locale(&self) -> Option<&dyn Locale> {
        Some(&PAGE_LOCALE)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_app_window::i18n::phrase(id, args)
    }

    fn now(&self) -> String {
        tilolang_tilo::instance::wall_now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        tilolang_tilo::instance::wall_moment()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        let wake = self.wake.clone();
        Arc::new(move || wake.wake())
    }
}

/// How often the window and its instance answer each other in one frame before the frame is
/// drawn: what the person did, the change it planned written down, and the views it moved.
#[cfg(target_arch = "wasm32")]
const EXCHANGES: usize = 8;

/// The window of the installed package, running its instance in the page. See the module docs.
#[cfg(target_arch = "wasm32")]
pub struct PageWindow {
    view: AppWindow,
    instance: Instance,
    /// The instance asked for the window to come forward.
    activation: Cell<bool>,
}

#[cfg(target_arch = "wasm32")]
impl PageWindow {
    /// The window of `bundle` for the person `me`, over a store of its own.
    #[must_use]
    pub fn new(me: &str, bundle: Arc<Bundle>) -> Self {
        let app = bundle.meta.app.clone();
        let (app_end, window_end) = local::pair();
        let platform = Rc::new(PagePlatform {
            bundle,
            wake: ui::wake::WakeHandle::new(),
        });
        let mut instance = Instance::new(
            Config {
                app: app.clone(),
                me: me.into(),
                mode: Mode::Live,
                device: None,
                store: tilolang_tilo::resources::ResourceStore::new(Rc::new(
                    tilolang_tilo::resources::LocalPort::in_memory(),
                )),
                root: RootView::default(),
                // What the app hands outside (a post, a mail) stays in the page: it has no
                // development log to write, and no connector to reach.
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
                link_home: None,
            },
            platform,
            Rc::new(tilolang_tilo::names::Installed),
            Box::new(app_end),
        );
        instance.sync();
        // The page is the document's only replica, and so its origin: the package's roots are made
        // and its migrations run here.
        if let Some(host) = instance.host() {
            let roots: Vec<String> = host
                .package(&app)
                .map(|package| {
                    package
                        .meta
                        .roots
                        .iter()
                        .cloned()
                        .chain(package.meta.named_roots.iter().map(|root| root.ty.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for ty in roots {
                if let Err(error) = host.root_resource(&app, &ty) {
                    tracing::error!(%error, "a root of the package could not be made");
                }
            }
        }
        let mut window = Self {
            view: AppWindow::new(Box::new(window_end), Vec::new()),
            instance,
            activation: Cell::new(false),
        };
        window.exchange();
        for scenario in STAGED.with(|held| std::mem::take(&mut *held.borrow_mut())) {
            window.instance.stage(Some(&scenario));
            window.exchange();
            if let Some(why) = window.instance.take_stage_failure() {
                tracing::error!(%why, "a scenario the page asked for did not stage");
            }
        }
        window
    }

    /// Let the window and its instance answer each other until neither has more to say. Whether
    /// anything moved.
    fn exchange(&mut self) -> bool {
        let mut moved = false;
        for _ in 0..EXCHANGES {
            self.view.flush();
            let mut changed = self.instance.update();
            changed |= self.view.receive();
            if self.view.take_activation() {
                self.activation.set(true);
            }
            if !changed {
                break;
            }
            moved = true;
        }
        moved
    }
}

/// The window is the framework's surface of the app: what a capture stages runs in its instance.
#[cfg(target_arch = "wasm32")]
impl ui::framework::ResourceSurface<crate::resource::Workspace> for PageWindow {
    fn present(&mut self, _state: &Rc<crate::resource::WorkspaceState>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        self.instance.stage(scenario);
        self.exchange();
    }
}

#[cfg(target_arch = "wasm32")]
impl ui::component_lifecycle::WindowActivation for PageWindow {
    fn take_window_activation_intent(&self) -> bool {
        self.activation.replace(false)
    }
}

/// The window is named by what it shows, as the app window's window names it.
#[cfg(target_arch = "wasm32")]
impl ui::component_lifecycle::WindowTitle for PageWindow {
    fn window_title(&self) -> Option<&str> {
        self.view.title()
    }
}

/// The window stands for the app window's window in the layout and in every walk of the widgets.
#[cfg(target_arch = "wasm32")]
impl ui::widget::Decorator for PageWindow {
    type Inner = dyn Widget;

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::component_lifecycle::WindowActivation>() {
            request.provide::<dyn ui::component_lifecycle::WindowActivation>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::WindowTitle>() {
            request.provide::<dyn ui::component_lifecycle::WindowTitle>(self);
            return;
        }
        Widget::provide(&self.view, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::component_lifecycle::WindowActivation>() {
            request.provide::<dyn ui::component_lifecycle::WindowActivation>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::WindowTitle>() {
            request.provide::<dyn ui::component_lifecycle::WindowTitle>(self);
            return;
        }
        Widget::provide_mut(&mut self.view, request);
    }

    fn inner(&self) -> &(dyn Widget + 'static) {
        &self.view
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut self.view
    }

    fn name(&self) -> &'static str {
        "TilolangPageWindow"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.view.receive();
        Widget::sync(&mut self.view, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // What the person did runs before what it shows is read.
        let moved = self.exchange();
        let child = Widget::update(&mut self.view, state, path);
        // A control may report while the window updates (a sidebar's pick): the instance answers
        // it in this frame, so the page it picked shows now, not after the next input.
        let answered = self.view.owes() && self.exchange();
        moved || child || answered
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.view, event, ctx);
        // What the person did goes to the instance at once, not on the next frame.
        self.view.flush();
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.view
            .draw_child(scene, transform, text, theme, presence);
    }

    fn needs_idle_sync(&self) -> bool {
        self.instance.owes_a_frame() || self.view.owes() || Widget::needs_idle_sync(&self.view)
    }

    fn has_pending_visual_work(&self) -> bool {
        self.instance.owes_a_frame() || Widget::has_pending_visual_work(&self.view)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let instance = self
            .instance
            .wakes_in()
            .and_then(|wait| ui::time::now().checked_add(wait));
        match (instance, Widget::next_wake_deadline(&self.view)) {
            (Some(instance), Some(view)) => Some(instance.min(view)),
            (instance, view) => instance.or(view),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        Widget::take_structure_dirty(&mut self.view)
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        Widget::take_structure_change(&mut self.view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores thread-local region formats, clock zone and i18n state (including an unset
    /// `person_locale` override) even if an assertion fails.
    struct ThreadLocaleGuard {
        prev_locale: String,
        prev_person: Option<String>,
    }

    impl ThreadLocaleGuard {
        fn enter(person: Option<&str>) -> Self {
            tilo_app_window::i18n::ensure();
            Self {
                prev_locale: ui::i18n::I18nManager::current_locale(),
                prev_person: ui::i18n::set_person_locale(person),
            }
        }
    }

    impl Drop for ThreadLocaleGuard {
        fn drop(&mut self) {
            let _ = ui::i18n::set_person_locale(self.prev_person.as_deref());
            ui::i18n::I18nManager::set_locale(&self.prev_locale);
            ui::format::region::install_here(None, None);
        }
    }

    /// The browser page host agrees with every other host on the shared table of dates, times,
    /// clocks, week starts and German typed dates, and moves `words_revision` on in-page display
    /// locale changes.
    #[test]
    fn a_page_packages_dates_week_and_language_agree_with_every_other_host() {
        use std::rc::Rc;
        use tilolang_tilo::instance::testing::{
            LocalAgreementHost, assert_platform_agrees, compile_probe,
        };

        let _guard = ThreadLocaleGuard::enter(Some("en-US"));
        ui::i18n::I18nManager::set_locale("en-US");

        let (compiled, actions) = compile_probe();
        let bundle = Arc::new(Bundle::new(&compiled, []));
        let platform = Rc::new(PagePlatform {
            bundle,
            wake: ui::wake::WakeHandle::new(),
        });
        let utc = ui::clock::Zone::of(ui::format::TimeZone::UTC);
        let mut harness = LocalAgreementHost::open(
            "PagePlatform",
            Rc::clone(&platform) as Rc<dyn Platform>,
            move |formats| {
                ui::format::region::install_here(Some(formats), Some(utc.clone()));
            },
            |locale| {
                let _ = ui::i18n::set_person_locale(Some(locale));
            },
        );
        assert_platform_agrees(&mut harness, &actions);

        let catalog_rev = || {
            platform
                .words_revision()
                .wrapping_sub(ui::format::region::revision())
        };
        let rev_before = catalog_rev();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_ne!(
            catalog_rev(),
            rev_before,
            "switching display locale moves words_revision"
        );
    }
}
