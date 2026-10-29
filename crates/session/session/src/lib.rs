//! The native Tilo session: a thin host, as the web page is one.
//!
//! The compositor ([`tilo_compositor`]) shows buffers and routes input. The
//! session gives it the desktop every host shows
//! ([`tilo_desktop::desktop_host`]): the desk (backdrop, launchpad, the
//! strips and window bars of the desktop surface) under the windows, and
//! the chrome (top bar, strip ribbon and rail, overview, menus) above them;
//! the window policy of the desk's strips ([`tilo_surface_host::desktop`]),
//! so each window is a tab of a strip; and the desktop's surface apps, each
//! an ordinary app hosted on a layer of its own: the notification banners
//! and quick settings. What the chrome asks of its host comes back as
//! [`DesktopAsk`]s, carried out here.

#![cfg(target_os = "linux")]

mod chrome;
mod panels;
mod search;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, OutputConfig, WindowPolicy};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk, DesktopChrome};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::Command;
use tilo_surface_host::{DesktopHost, Placement, WidgetHost, desktop_with};
use ui::application::ComponentRunnerInputs;
use ui::headless::FrameHarness;
use ui::host_operation::ResourceOperationStart;

pub use search::{AppActions, SearchOptions};
pub use tilo_quick_settings::host::PanelHost;

use chrome::{SessionChrome, SessionRules};
use panels::{Banners, Held, Popover};
use search::SessionSearch;

/// The generation the session's own desktop commands carry.
const GENERATION: &str = "session";

/// What a session starts with.
pub struct Options {
    /// Seeds the desktop's ids, so two devices never mint the same.
    pub salt: u16,
    /// Physical pixels per logical pixel the desktop draws at.
    pub scale: f64,
    /// This device, by id.
    pub device: String,
    /// The person at it, by the id the desktop's authors name them by.
    pub person: String,
    /// The theme and fonts surface apps draw with.
    pub presentation: fn() -> ComponentRunnerInputs,
    /// What quick settings reads of the device and carries out on it;
    /// without one, the bar's status cluster opens nothing.
    pub quick_settings: Option<Rc<dyn PanelHost>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The action host: what open apps publish and run, and where what is
    /// chosen in search runs by address. Without one, search finds the
    /// desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            salt: 1,
            scale: 1.0,
            device: "this-device".into(),
            person: "person".into(),
            presentation: ComponentRunnerInputs::from_native_process,
            quick_settings: None,
            search: SearchOptions::default(),
            apps: None,
        }
    }
}

/// The desktop's two hosted trees: the desk's under the windows and the
/// chrome's above them.
#[derive(Clone)]
struct Layers {
    desktop: DesktopHost,
    chrome: Rc<RefCell<WidgetHost>>,
}

impl Layers {
    /// `read` of the desk's hosted tree, then of the chrome's.
    fn each<R>(&self, mut read: impl FnMut(&mut FrameHarness) -> R) -> [R; 2] {
        let desk = self.desktop.with_widgets(|host| read(host.host_mut()));
        let chrome = read(self.chrome.borrow_mut().host_mut());
        [desk, chrome]
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        self.each(|host| host.resources()).concat()
    }

    /// Invoke the published action `action` of `resource` on the layer
    /// that publishes it.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let address = ActionAddress::new(resource, action);
        let [desk, chrome] = self.each(|host| {
            tilo_actions::door::resolve(&host.resources(), &address)
                .is_some()
                .then(|| host.invoke_published(resource, action, arguments))
        });
        desk.or(chrome)
            .unwrap_or_else(|| Err(DoorError::Unknown(address.clone())))
    }
}

/// What the session keeps between frames, shared with the loop.
struct Shared {
    layers: Layers,
    handle: DesktopHandle,
    search: Rc<SessionSearch>,
    /// What the desk and the chrome asked of the session.
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether quick settings is open, when the session has it.
    quick_settings: Option<Rc<Cell<bool>>>,
}

impl Shared {
    /// Carry out what the desktop asked since the last frame.
    fn follow_asks(&self) {
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for ask in asks {
            match ask {
                DesktopAsk::Intent(ChromeIntent::ToggleQuickSettings) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(!open.get());
                    }
                }
                DesktopAsk::Intent(ChromeIntent::ShowQuickSettings) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(true);
                    }
                }
                DesktopAsk::Launchpad(action) => self.launchpad(action),
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        let choices = self.handle.take_search_choices();
        if choices.is_empty() {
            return;
        }
        let room = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
                .unwrap_or_default()
        });
        for choice in choices {
            self.search.perform(&room, choice, &mut |own| {
                self.layers
                    .invoke(
                        own.address.resource.as_str(),
                        own.address.action.as_str(),
                        &own.arguments,
                    )
                    .map(drop)
            });
        }
        // What the session's own actions asked of it.
        self.follow_asks();
    }

    /// Read what the session publishes for search, after a frame.
    fn observe(&self) {
        let open_tabs = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect()
        });
        self.search.observe(&self.layers.resources(), open_tabs);
    }

    /// A card of the empty desk's launchpad: its app, else the desktop's
    /// search, which finds what to open.
    fn launchpad(&self, action: LaunchpadAction) {
        match action {
            LaunchpadAction::Activity => self
                .intents
                .borrow_mut()
                .push(ChromeIntent::ShowActivity(None)),
            other => match other.app_target() {
                Some((app, _)) => {
                    let launch = Command::Launch {
                        generation: GENERATION.into(),
                        app: app.into(),
                    };
                    if let Err(error) = self.handle.dispatch(&launch, GENERATION) {
                        tracing::warn!(%error, app, "launchpad");
                    }
                }
                None => self.handle.open_search(None),
            },
        }
    }
}

/// A running session: the compositor and the desktop it hosts.
pub struct Session {
    compositor: Compositor,
    shared: Rc<Shared>,
    banners: tilo_notification_banners::Link,
}

impl Session {
    /// A session on the compositor `start` makes with the desktop's window
    /// policy.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn new(
        options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, Error> {
        // What the desk and the chrome ask of the session, from their own
        // widget trees.
        let asks = Rc::new(RefCell::new(Vec::new()));
        let (desktop, policy) = desktop_with(options.salt, options.scale, |surface, handle| {
            Box::new(
                DeskLayer::new(
                    Box::new(surface),
                    handle.clone(),
                    CollaborationLaunchpad::new(),
                    Vec::new(),
                )
                .with_asks(Rc::clone(&asks)),
            )
        });
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        let mut compositor = start(Box::new(policy))?;
        compositor.add_host(Box::new(desktop.clone()));

        // The chrome above the windows: its own tree, beside the desk's.
        let rules = Rc::new(SessionRules {
            handle: handle.clone(),
            person: options.person.clone(),
            authors: RefCell::new(BTreeMap::new()),
        });
        let tree = handle.read(Clone::clone);
        // The bar's source hands its intents to the chrome it sits in.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let source = Rc::new(SessionChrome::new(
            handle.clone(),
            Rc::clone(&intents),
            options.person.clone(),
        ));
        let bar = TopBarChrome::from_tree(&tree)
            .with_chrome(source)
            .with_tilo_mark();
        let chrome = DesktopChrome::new(
            handle.clone(),
            GENERATION.into(),
            rules,
            bar,
            TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
            false,
        )
        .with_intents(Rc::clone(&intents))
        .with_asks(Rc::clone(&asks));
        let keyboard = chrome.keyboard();
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale: options.scale,
        };
        let chrome = Rc::new(RefCell::new(
            WidgetHost::new(Box::new(chrome), placement, false)
                .covering_outputs()
                .with_keyboard(keyboard),
        ));
        compositor.add_host(Box::new(Held(Rc::clone(&chrome))));

        let presentation = options.presentation;
        let banners = tilo_notification_banners::Link::new();
        match tilo_notification_banners::prepare(banners.clone(), &options.device, &options.person)
            .and_then(|component| ui::App::from_component(component, presentation()))
        {
            Ok(app) => {
                compositor.add_host(Box::new(Banners::new(app, banners.clone(), options.scale)));
            }
            Err(error) => tracing::warn!(%error, "notification banners not mounted"),
        }

        let quick_settings = options.quick_settings.and_then(|panel| {
            let open = Rc::new(Cell::new(false));
            match tilo_quick_settings::prepare(panel, &options.device, &options.person, 1)
                .and_then(|component| ui::App::from_component(component, presentation()))
            {
                Ok(app) => {
                    compositor.add_host(Box::new(Popover::new(
                        app,
                        Rc::clone(&open),
                        options.scale,
                    )));
                    Some(open)
                }
                Err(error) => {
                    tracing::warn!(%error, "quick settings not mounted");
                    None
                }
            }
        });

        let search = Rc::new(SessionSearch::new(options.search, options.apps));
        handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
        let shared = Rc::new(Shared {
            layers: Layers { desktop, chrome },
            handle,
            search,
            asks,
            intents,
            quick_settings,
        });
        shared.observe();
        Ok(Self {
            compositor,
            shared,
            banners,
        })
    }

    /// A session whose outputs have no display: for tests and screenshots.
    ///
    /// # Errors
    /// The compositor or an output did not start.
    pub fn headless(options: Options, outputs: &[OutputConfig]) -> Result<Self, Error> {
        let mut session = Self::new(options, Compositor::headless)?;
        for output in outputs {
            session.compositor.add_output(output)?;
        }
        Ok(session)
    }

    /// A session on a bare TTY.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn drm(options: Options, config: DisplayConfig) -> Result<Self, Error> {
        Self::new(options, |policy| Compositor::drm(policy, config))
    }

    /// The compositor, to connect clients, route input and read frames.
    pub fn compositor(&mut self) -> &mut Compositor {
        &mut self.compositor
    }

    /// The desktop under the windows and its policy.
    #[must_use]
    pub fn desktop(&self) -> &DesktopHost {
        &self.shared.layers.desktop
    }

    /// The host side of the desktop surface.
    #[must_use]
    pub fn handle(&self) -> &DesktopHandle {
        &self.shared.handle
    }

    /// The notification banners' link: the notifications service publishes
    /// on it and carries out what the banners ask.
    #[must_use]
    pub const fn banners(&self) -> &tilo_notification_banners::Link {
        &self.banners
    }

    /// Everything the desktop publishes now: the desk's resource (its
    /// windows and the open search among its children) and the chrome's,
    /// with their actions.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.shared.layers.resources()
    }

    /// A window was at `place`: where search's Continue reopens it.
    pub fn record_place(&self, place: tilo_desktop_search::providers::places::Place) {
        self.shared.search.record_place(place);
    }

    /// Invoke the published action `action` of resource `resource`, as
    /// any caller of the action protocol does, and carry out what it asked
    /// of the session.
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, or bad arguments.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let started = self.shared.layers.invoke(resource, action, arguments)?;
        self.shared.follow_asks();
        Ok(started)
    }

    /// The labels of the desktop's accessibility trees, the desk's and the
    /// chrome's, as assistive technology reads them.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.shared
            .layers
            .each(|host| {
                host.a11y_nodes()
                    .into_iter()
                    .filter_map(|(_, node)| node.label().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .concat()
    }

    /// Whether quick settings is open.
    #[must_use]
    pub fn quick_settings_open(&self) -> bool {
        self.shared
            .quick_settings
            .as_ref()
            .is_some_and(|open| open.get())
    }

    /// One turn of the loop: wait up to `timeout` for clients and devices,
    /// carry out what the desktop asked, and draw.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn step(&mut self, timeout: std::time::Duration) -> Result<(), Error> {
        self.compositor.dispatch(timeout)?;
        self.shared.follow_asks();
        self.compositor.render()?;
        self.shared.follow_asks();
        self.shared.observe();
        Ok(())
    }

    /// Run until `stop` holds.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        let shared = Rc::clone(&self.shared);
        self.compositor.run(|compositor| {
            shared.follow_asks();
            shared.observe();
            stop(compositor)
        })
    }
}
