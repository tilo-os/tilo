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

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

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

pub use tilo_quick_settings::host::PanelHost;

use chrome::{SessionChrome, SessionRules};
use panels::{Banners, Held, Popover};

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
        }
    }
}

/// What the session keeps between frames, shared with the loop.
struct Shared {
    handle: DesktopHandle,
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
    desktop: DesktopHost,
    /// The chrome above the windows.
    chrome: Rc<RefCell<WidgetHost>>,
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
                compositor.add_host(Box::new(Banners::new(app, banners.clone(), options.scale)))
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

        let shared = Rc::new(Shared {
            handle,
            asks,
            intents,
            quick_settings,
        });
        Ok(Self {
            compositor,
            desktop,
            chrome,
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
    pub const fn desktop(&self) -> &DesktopHost {
        &self.desktop
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

    /// The names the chrome above the windows gives what it shows, as
    /// assistive technology reads them.
    #[must_use]
    pub fn chrome_names(&self) -> Vec<String> {
        self.chrome
            .borrow_mut()
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
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
            stop(compositor)
        })
    }
}
