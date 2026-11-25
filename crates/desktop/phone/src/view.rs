//! The phone's window: the shell's `phone` view, run in an instance in this process over what the
//! phone's device reads ([`PhoneOrigin`]), with what it asks carried out by that device
//! ([`PhoneInvoker`]). An app the person opens takes the window, in tilolang-process's in-process
//! window, as `tilolang dev` runs one; Back returns to the shell.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Locale, Mode, Platform};
use tilolang_tilo::origin::{Origin, OriginCx, Polled};
use tilolang_tilo::resources::{LocalPort, ResourceStore};
use tilolang_tilo::runtime::ResourceRef;
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::value::Value;
use tilolang_window::Packages;
use ui::widget::{AppModel, DrawChild as _, ModelPath, Widget};

use crate::worker::{Ask, Seen, Worker};

/// How often the window and its instance may answer each other in one frame: a press, the action
/// it ran, the reading that moved, what that shows.
const EXCHANGES: usize = 8;

/// The person at the phone, as the shell's instance names whoever holds the device.
const ME: &str = "me";

/// What the shell's view reads of the device, by the id the session publishes each under.
const DEVICES: &str = "session:devices";
const APPS: &str = "session:apps";

/// The window. See the module docs.
pub struct PhoneView {
    instance: Instance,
    shell: AppWindow,
    /// The app the person opened, running in this process, and its id.
    opened: Option<(String, tilolang_process::window::hosted::HostedWindow)>,
    worker: Worker,
    /// What the invoker asked to open, taken on the next frame.
    to_open: Rc<RefCell<Option<String>>>,
    /// The bundles the APK carries, by app id.
    bundles: BTreeMap<String, Arc<Bundle>>,
    /// Where the apps' documents are kept on this device.
    storage: Option<PathBuf>,
}

impl PhoneView {
    /// The shell's `phone` view over `worker`'s device, opening the apps of `bundles`, keeping
    /// their documents under `storage`.
    ///
    /// # Errors
    /// The shell does not compile, or its view does not start.
    pub fn new(
        worker: Worker,
        bundles: BTreeMap<String, Arc<Bundle>>,
        storage: Option<PathBuf>,
        waker: Waker,
    ) -> Result<Self, String> {
        let compiled = tilolang_tilo::shell::compile()?;
        let platform = tilolang_tilo::platform_apps::platform_packages()?;
        let to_open = Rc::new(RefCell::new(None));
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: tilolang_tilo::shell::SHELL_APP.to_owned(),
                me: ME.into(),
                mode: Mode::Live,
                device: None,
                store: ResourceStore::new(Rc::new(LocalPort::in_memory())),
                root: RootView {
                    root: None,
                    view: Some("phone".to_owned()),
                },
                sink: None,
                // A phone serves no web page of the workspace to link to.
                link_home: None,
            },
            Rc::new(PhonePlatform {
                packages: Packages::new(Rc::new(compiled), platform),
                invoker: PhoneInvoker {
                    worker: worker.clone(),
                    to_open: Rc::clone(&to_open),
                },
                waker,
            }),
            Rc::new(tilolang_tilo::names::Installed),
            Box::new(app_end),
        );
        instance.add_origin(Rc::new(RefCell::new(PhoneOrigin {
            worker: worker.clone(),
            written: None,
        })));
        instance.sync();
        let mut shell = AppWindow::new(Box::new(window_end), Vec::new());
        shell.receive();
        if let Some(why) = shell.failed() {
            return Err(format!("the shell's window refused what it sent: {why}"));
        }
        let mut view = Self {
            instance,
            shell,
            opened: None,
            worker,
            to_open,
            bundles,
            storage,
        };
        view.exchange();
        Ok(view)
    }

    /// Let the shell's window and its instance answer each other until neither has more to say.
    fn exchange(&mut self) -> bool {
        let mut any = false;
        for _ in 0..EXCHANGES {
            self.instance.settle_pending();
            self.shell.flush();
            let moved = self.instance.update();
            let received = self.shell.receive();
            any |= moved || received;
            if !moved && !received && !self.shell.owes() && !self.instance.pending() {
                break;
            }
        }
        any
    }

    /// Open what the shell asked to open, in the workspace the person chose.
    fn open_asked(&mut self) -> bool {
        let Some(app) = self.to_open.borrow_mut().take() else {
            return false;
        };
        let Seen {
            workspace, devices, ..
        } = self.worker.seen();
        let workspace = workspace.unwrap_or_default();
        // Once a person's identity has the device, what an app keeps is the workspace's: its
        // window joins it over the connection the device's daemon makes for it. Before that
        // nothing is anyone's, and the app's records stay on the phone.
        let records = if devices.signed_in {
            let (asked, wanted) = (app.clone(), workspace.clone());
            match self
                .worker
                .files()
                .ask(move |device| device.window_connection(&asked, &wanted))
                .and_then(|made| made.map_err(|error| format!("{error:#}")))
            {
                Ok(connection) => Records::Workspace(connection),
                Err(error) => {
                    tracing::error!(app, %error, "the app's window has no connection to its workspace");
                    return false;
                }
            }
        } else {
            Records::Here(self.storage.clone())
        };
        match open(&app, &workspace, &self.bundles, records) {
            Ok(window) => {
                self.opened = Some((app, window));
                true
            }
            Err(error) => {
                tracing::error!(app, %error, "the app did not start");
                false
            }
        }
    }

    /// Whether an app is open over the shell.
    #[must_use]
    pub fn opened(&self) -> Option<&str> {
        self.opened.as_ref().map(|(app, _)| app.as_str())
    }

    /// Back: close the app shown, and show the shell again.
    pub fn back(&mut self) -> bool {
        self.opened.take().is_some()
    }
}

impl ui::widget::Decorator for PhoneView {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        match &self.opened {
            Some((_, window)) => window,
            None => &self.shell,
        }
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.opened {
            Some((_, window)) => window,
            None => &mut self.shell,
        }
    }

    fn name(&self) -> &'static str {
        "PhoneView"
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.instance.sync();
        let exchanged = self.exchange();
        // A tap on one of the phone's notifications opens what the thing it is about is in.
        if let Some(app) = self.worker.take_open() {
            *self.to_open.borrow_mut() = Some(app);
        }
        let opened = self.open_asked();
        let child = self.inner_mut().update(state, path);
        exchanged || opened || child || self.shell.owes()
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // Android's Back closes the app shown over the shell.
        if let ui::winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state.is_pressed()
            && key.logical_key
                == ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::GoBack)
            && self.back()
        {
            return;
        }
        self.inner_mut().handle_event(event, ctx);
        if self.opened.is_none() {
            self.shell.flush();
        }
    }

    fn draw(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        let (scene, transform, text, theme, env) = ctx.parts();
        self.inner_mut()
            .draw_child(scene, transform, text, theme, &env);
    }

    fn needs_idle_sync(&self) -> bool {
        self.instance.pending() || self.inner().needs_idle_sync()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let instance = self
            .instance
            .wakes_in()
            .and_then(|wait| ui::time::now().checked_add(wait));
        match (instance, self.inner().next_wake_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// Where an app the phone opens keeps its records.
pub enum Records {
    /// On the phone, under the path given (in memory without one): only before a person's
    /// identity has the device, when there is no workspace for them to be in.
    Here(Option<PathBuf>),
    /// In the workspace, over the connection the device's daemon made for the app's window
    /// ([`tilo_runtime::in_process::Device::window_connection`]): kept in its one store and
    /// synced to the person's other devices, as any device's window keeps them.
    Workspace(tilo_runtime::in_process::WindowConnection),
}

/// The app `app` of `workspace`, run in this process: the bundle the APK carries, or the tilolang
/// package this device installed from the workspace and verified as a launch of it verifies it,
/// shown in tilolang-process's in-process window, keeping its records where `records` says. One
/// path for either: the phone starts no process, and a fetched app is as much the app's own
/// threads as a carried one.
///
/// # Errors
/// The phone carries no bundle of the app and holds no verified install of it, or the app's
/// instance does not start or join its workspace.
pub fn open(
    app: &str,
    workspace: &str,
    bundles: &BTreeMap<String, Arc<Bundle>>,
    records: Records,
) -> Result<tilolang_process::window::hosted::HostedWindow, String> {
    // A thing told names the app that holds what it is about by its id, which a telling written
    // elsewhere may spell whole (`so.tilo.notes`) where the phone carries `notes`.
    let found = bundles.get(app).or_else(|| {
        let last = app.rsplit('.').next().unwrap_or_default();
        bundles.get(last)
    });
    let bundle = match found {
        Some(bundle) => Arc::clone(bundle),
        None => {
            let payload = tilo_runtime::device::package_runs::verified_payload(app)?;
            Arc::new(Bundle::read(&payload).map_err(|error| format!("{app}: {error}"))?)
        }
    };
    let id = uuid::Uuid::parse_str(workspace)
        .unwrap_or_else(|_| uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, workspace.as_bytes()));
    let opened = match records {
        Records::Here(storage) => {
            let route = tilolang_process::route_of(&bundle.meta, id, ME);
            let spec =
                tilolang_process::hosted::Spec::new(bundle.meta.app.clone(), workspace, 1, ME);
            tilolang_process::window::hosted::HostedWindow::in_process(spec, route, bundle, storage)
        }
        Records::Workspace(connection) => {
            // The person at the window is the one the device acts for: who their records and
            // their own workspace's documents are kept as.
            let person = connection.person;
            let route = tilolang_process::route_of(&bundle.meta, id, &person);
            let spec = tilolang_process::hosted::Spec::new(
                bundle.meta.app.clone(),
                workspace,
                connection.instance,
                &person,
            );
            tilolang_process::window::hosted::HostedWindow::in_process_joined(
                spec,
                route,
                bundle,
                (connection.collaboration, connection.identity),
            )
        }
    };
    opened.map_err(|error| format!("{app} did not start: {error}"))
}

/// The phone's device as the origin of what the shell's view reads of it.
struct PhoneOrigin {
    worker: Worker,
    /// The generation of what it last wrote.
    written: Option<u64>,
}

impl Origin for PhoneOrigin {
    fn name(&self) -> &'static str {
        "phone"
    }

    fn serves(&self, _capability: &str) -> bool {
        false
    }

    fn due(&self) -> bool {
        self.written != Some(self.worker.seen().generation)
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let seen = self.worker.seen();
        if self.written == Some(seen.generation) {
            return Polled::default();
        }
        let devices = serde_json::json!({
            "signed_in": seen.devices.signed_in,
            "workspaces": seen.devices.workspaces,
            "refused": seen.refused.unwrap_or_default(),
        });
        let apps = serde_json::json!({ "installed": seen.apps });
        for (id, ty, reading) in [(DEVICES, "Devices", devices), (APPS, "Apps", apps)] {
            let ty = format!("{}#{ty}", tilolang_tilo::platform_apps::DEVICE_APP);
            let Some(target) = ResourceRef::parse_id(id, &ty) else {
                tracing::error!(id, "the phone's reading names no resource");
                continue;
            };
            if let Err(error) = cx.host.runtime().set_reading(&target, &reading.to_string()) {
                tracing::error!(id, %error, "the phone's reading was not taken");
            }
        }
        self.written = Some(seen.generation);
        (cx.waker)();
        Polled::default()
    }

    fn deliver(
        &mut self,
        capability: &str,
        _payload: &serde_json::Value,
    ) -> tilolang_tilo::runtime::Delivery {
        tilolang_tilo::runtime::Delivery::Rejected(format!("the phone serves no `{capability}`"))
    }
}

/// What the shell's view asks on a phone, carried out by its device: a login, the workspace the
/// person chose, and the app they open. Nothing else is the phone's to do: anything else is
/// refused in words, never dropped.
struct PhoneInvoker {
    worker: Worker,
    to_open: Rc<RefCell<Option<String>>>,
}

impl PhoneInvoker {
    fn argument(invocation: &Invocation, name: &str) -> Option<String> {
        invocation
            .api_arguments()
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}

impl Invoker for PhoneInvoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action { name, .. } = invocation else {
            return Invoked::Failed(
                "a phone's shell keeps no record of its own to change".to_owned(),
            );
        };
        let asked = |field: &str| {
            Self::argument(invocation, field)
                .ok_or_else(|| format!("`{name}` was asked without its `{field}`"))
        };
        let answer = match name.as_str() {
            "log-in" => asked("ticket").map(|ticket| self.worker.ask(Ask::LogIn(ticket))),
            "found" => {
                self.worker.ask(Ask::Found);
                Ok(())
            }
            "switch-workspace" => {
                asked("workspace").map(|workspace| self.worker.ask(Ask::Workspace(workspace)))
            }
            "open-app" => asked("app").map(|app| {
                *self.to_open.borrow_mut() = Some(app);
            }),
            other => Err(format!("`{other}` is not something a phone does")),
        };
        match answer {
            Ok(()) => Invoked::Done(Value::Unit),
            Err(why) => Invoked::Failed(why),
        }
    }
}

/// The shell's packages and the platform's, as a page loads them, with the phone's invoker: what
/// the shell's view asks goes to the phone's device.
struct PhonePlatform {
    packages: Packages,
    invoker: PhoneInvoker,
    waker: Waker,
}

impl Platform for PhonePlatform {
    fn load(&self, host: &Host) -> u64 {
        let loaded = self.packages.load(host);
        host.runtime().set_invoker(PhoneInvoker {
            worker: self.invoker.worker.clone(),
            to_open: Rc::clone(&self.invoker.to_open),
        });
        loaded
    }

    fn locale(&self) -> Option<&dyn Locale> {
        self.packages.locale()
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        self.packages.phrase(id, args)
    }

    fn now(&self) -> String {
        self.packages.now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        self.packages.moment()
    }

    fn waker(&self) -> Waker {
        Arc::clone(&self.waker)
    }
}

/// The framework hosts the phone over a workspace resource it keeps nothing in: what the phone
/// shows is the device's, read through its daemon.
impl ui::framework::ResourceSurface<tilolang_process::window::resource::Workspace> for PhoneView {
    fn present(&mut self, _state: &Rc<tilolang_process::window::resource::WorkspaceState>) {}
}
