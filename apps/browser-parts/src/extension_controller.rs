//! Extensions in the browser window: the Web Store install offer, the
//! toolbar's action buttons and popups, and the Library page's manager.
//!
//! The controller owns no widget state the document needs; it is the
//! window's local, device-only view of the profile's extensions. Interface
//! closures queue [`Command`]s, `sync` runs them and starts the slow parts
//! (download, verify, unpack) on worker threads whose results come back
//! through [`Outcome`]s, so nothing here blocks a frame. The profile itself
//! (which extensions are installed, approved and pinned, their manifests and
//! icons) is read and written on one worker of its own, in order, since the
//! window may be drawn on the compositor's frame loop.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use ui::icons::Icon;
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::floating_surface::PanelAlign;
use ui::widget::{
    Actions, Banner, Button, Column, Container, Element, ListRow, MenuButton, MenuItem, Text,
    TextInput,
};
use ui::window_toolbar::ToolbarAction;

use crate::CATALOG;
use crate::extension_host::{ExtensionHost, Wanted};
use crate::extension_toolbar::{self as toolbar, Installed};
use crate::extensions::{self, ExtensionInfo, StagedPackage};
use crate::web_store;

/// An extension popup's size before its page says how big it is, in
/// logical pixels: as wide as a panel, and short, because a page reports at
/// least the view it is laid out in, so a popup can only grow to its page.
const POPUP_SIZE: (f32, f32) = (360.0, 120.0);
/// The largest popup, as in Chrome.
const POPUP_MAX: (f32, f32) = (800.0, 600.0);

const ADD_BUTTON: &str = "browser.extension.add";
const MENU_BUTTON: &str = "browser.extensions.menu";

/// The window bar's action ids: the Web Store offer, and the prefix of an
/// item opening one extension's popup.
const BAR_ADD: &str = "extension-add";
const BAR_OPEN: &str = "extension-open:";

/// Something the interface asked for.
#[derive(Debug, Clone)]
enum Command {
    /// Fetch a Web Store item and show its permissions.
    Fetch(String),
    /// Read a package file and show its permissions.
    StageFile(PathBuf),
    /// Install what is being offered.
    Confirm,
    Cancel,
    Enable(String),
    Disable(String),
    Remove(String),
    Pin(String, bool),
    OpenAction(String),
    ClosePanel,
}

/// The extensions whose badges the window shows, so whose state is read:
/// in a Tilo window's bar every extension with an action is an item with its
/// badge; a window of its own shows only the pinned ones in its row.
fn badges_shown(installed: &[Installed], hosted: bool) -> Vec<String> {
    installed
        .iter()
        .filter(|extension| extension.enabled && extension.action.is_some())
        .filter(|extension| hosted || extension.pinned)
        .map(|extension| extension.id.clone())
        .collect()
}

/// A slow step's result.
enum Outcome {
    Staged(Result<StagedPackage, String>),
    Installed(Result<ExtensionInfo, String>),
    /// A profile job ran: the change it made refused, if it did, and the
    /// profile as it then was.
    Profile {
        refused: Option<String>,
        profile: Profile,
    },
}

/// A change to the profile, made on the profile's worker.
type ProfileChange = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// One job for the profile's worker: a change to make, if any, then a read
/// of the profile, reading the icons of extensions not in `known`.
struct ProfileJob {
    change: Option<ProfileChange>,
    known: HashSet<String>,
}

/// The profile's extensions as one read found them.
struct Profile {
    infos: Vec<ExtensionInfo>,
    installed: Vec<Installed>,
    wanted: Vec<Wanted>,
    unwanted: Vec<String>,
    /// Icons of extensions the read was told it did not know yet.
    icons: Vec<(String, Option<vello::peniko::ImageData>)>,
}

/// Read the profile, with the icons of the extensions not in `known`.
fn read_profile(known: &HashSet<String>) -> Profile {
    let infos = extensions::list_installed();
    let installed: Vec<Installed> = infos
        .iter()
        .filter_map(|info| {
            Installed::read(
                info,
                extensions::is_approved(info),
                extensions::is_pinned(&extensions::chrome_id(info)?),
            )
        })
        .collect();
    let wanted = installed
        .iter()
        .filter(|extension| extension.enabled)
        .filter_map(|extension| {
            Some(Wanted {
                id: extension.id.clone(),
                path: extension.root.canonicalize().ok()?,
                name: extension.name.clone(),
            })
        })
        .collect();
    let unwanted = installed
        .iter()
        .filter(|extension| !extension.enabled)
        .map(|extension| extension.id.clone())
        .collect();
    let icons = installed
        .iter()
        .filter(|extension| !known.contains(&extension.id))
        .map(|extension| {
            let icon = extension
                .icon_path()
                .and_then(|path| toolbar::read_icon(&path));
            (extension.id.clone(), icon)
        })
        .collect();
    Profile {
        infos,
        installed,
        wanted,
        unwanted,
        icons,
    }
}

/// What the anchored panel shows.
enum Panel {
    Offer(Box<StagedPackage>),
    Message(String),
    Popup {
        id: String,
        engine: SharedEngine,
        /// The size its page asked for, once it said.
        size: Option<(f32, f32)>,
    },
}

pub struct ExtensionController {
    /// Whether this window manages extensions: not a private or headless one.
    active: bool,
    commands: Arc<Mutex<Vec<Command>>>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    /// The profile's worker, once started.
    profile_jobs: Option<Sender<ProfileJob>>,
    host: ExtensionHost,
    installed: Vec<Installed>,
    infos: Vec<ExtensionInfo>,
    icons: HashMap<String, Option<vello::peniko::ImageData>>,
    refresh: bool,
    store_item: Option<web_store::Item>,
    busy: bool,
    panel: Option<Panel>,
    file_path: ui::LocalState<String>,
    chromium_version: String,
}

impl ExtensionController {
    pub fn new(active: bool) -> Self {
        Self {
            active,
            commands: Arc::default(),
            outcomes: Arc::default(),
            profile_jobs: None,
            host: ExtensionHost::new(),
            installed: Vec::new(),
            infos: Vec::new(),
            icons: HashMap::new(),
            refresh: active,
            store_item: None,
            busy: false,
            panel: None,
            file_path: ui::LocalState::new(String::new()),
            chromium_version: web_store::FALLBACK_CHROMIUM_VERSION.to_owned(),
        }
    }

    fn queue(&self, command: Command) -> impl FnMut() + 'static {
        let commands = self.commands.clone();
        move || {
            if let Ok(mut queue) = commands.lock() {
                queue.push(command.clone());
            }
        }
    }

    /// The panel was dismissed by a click outside or Escape.
    pub fn dismiss(&mut self) {
        self.panel = None;
    }

    /// A reply from the engine to one of this controller's `DevTools` calls.
    pub fn on_result(&mut self, request_id: u32, ok: bool, body: &str) -> bool {
        let update = self.host.on_result(request_id, ok, body, ui::time::now());
        if let Some(notice) = update.notice {
            self.panel = Some(Panel::Message(notice));
            return true;
        }
        update.changed
    }

    /// Run queued commands, collect finished work and keep the engine's set
    /// of running extensions equal to the enabled ones. Returns whether the
    /// interface changed.
    pub fn sync(&mut self, engine: &SharedEngine, current_uri: &str) -> bool {
        if !self.active {
            return false;
        }
        let mut changed = false;
        let item = web_store::item_from_url(current_uri);
        if item != self.store_item {
            self.store_item = item;
            changed = true;
        }
        let commands: Vec<Command> = self
            .commands
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        for command in commands {
            self.run(command, engine);
            changed = true;
        }
        let outcomes: Vec<Outcome> = self
            .outcomes
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        for outcome in outcomes {
            changed = true;
            match outcome {
                Outcome::Profile { refused, profile } => {
                    if let Some(error) = refused {
                        self.panel = Some(Panel::Message(error));
                    }
                    self.apply_profile(profile);
                    continue;
                }
                Outcome::Staged(Ok(package)) => {
                    self.panel = Some(Panel::Offer(Box::new(package)));
                    self.refresh = true;
                }
                Outcome::Installed(Ok(info)) => {
                    self.panel = Some(Panel::Message(format!("{} was added", info.name)));
                    self.change_profile(move || {
                        extensions::set_approved(&info, true).map_err(|error| error.to_string())
                    });
                }
                Outcome::Staged(Err(error)) | Outcome::Installed(Err(error)) => {
                    self.panel = Some(Panel::Message(format!(
                        "Could not add the extension: {error}"
                    )));
                    self.refresh = true;
                }
            }
            self.busy = false;
        }
        if self.refresh {
            self.refresh = false;
            self.read_profile();
        }
        let now = ui::time::now();
        let shown = badges_shown(&self.installed, ui::window_toolbar::is_hosted());
        self.host.probe(&shown, now);
        self.host.flush(engine);
        if let Some(Panel::Popup {
            engine: popup,
            size,
            ..
        }) = &mut self.panel
        {
            // The popup is a tab of its own: its page events are not the
            // window's, but they must not pile up.
            let mut closed = false;
            for event in popup.drain_events() {
                match event {
                    // Its page closed it (a password filled, a choice made).
                    ui::webview::WebViewEvent::TabClosed => closed = true,
                    // As big as its page, as Chrome sizes a popup.
                    ui::webview::WebViewEvent::DocumentSize { width, height, .. } => {
                        let page = (
                            ui::num::f32_of_f64(width).clamp(POPUP_SIZE.0, POPUP_MAX.0),
                            ui::num::f32_of_f64(height).clamp(25.0, POPUP_MAX.1),
                        );
                        if *size != Some(page) {
                            *size = Some(page);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            // What it opens (its dashboard, a help page) opens beside the
            // window, as Chrome opens it in a tab.
            for (url, background) in popup.take_new_tab_requests() {
                if url.starts_with("https://") || url.starts_with("http://") {
                    ui::shell_tabs::open_beside(Some(url), background);
                }
            }
            if closed {
                self.panel = None;
                changed = true;
            }
        }
        changed
    }

    /// Have the profile's worker read the profile.
    fn read_profile(&mut self) {
        self.profile_job(None);
    }

    /// Have the profile's worker make `change`, then read the profile.
    fn change_profile(&mut self, change: impl FnOnce() -> Result<(), String> + Send + 'static) {
        self.profile_job(Some(Box::new(change)));
    }

    fn profile_job(&mut self, change: Option<ProfileChange>) {
        let job = ProfileJob {
            change,
            known: self.icons.keys().cloned().collect(),
        };
        let job = match &self.profile_jobs {
            Some(jobs) => match jobs.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (jobs, queue) = mpsc::channel::<ProfileJob>();
        let outcomes = self.outcomes.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-extensions".into())
            .spawn(move || {
                for job in std::iter::once(job).chain(queue) {
                    let refused = job.change.and_then(|change| change().err());
                    let profile = read_profile(&job.known);
                    if let Ok(mut queue) = outcomes.lock() {
                        queue.push(Outcome::Profile { refused, profile });
                    }
                }
            });
        match spawned {
            Ok(_) => self.profile_jobs = Some(jobs),
            Err(error) => {
                self.panel = Some(Panel::Message(format!(
                    "Extensions could not be read: {error}"
                )));
            }
        }
    }

    /// Bring the engine and the window in line with a read of the profile.
    fn apply_profile(&mut self, profile: Profile) {
        self.host
            .reconcile(&profile.wanted, &profile.unwanted, ui::time::now());
        self.icons
            .retain(|id, _| profile.installed.iter().any(|e| &e.id == id));
        self.icons.extend(profile.icons);
        self.installed = profile.installed;
        self.infos = profile.infos;
    }

    fn info_of(&self, id: &str) -> Option<&ExtensionInfo> {
        self.infos
            .iter()
            .find(|info| extensions::chrome_id(info).as_deref() == Some(id))
    }

    fn run(&mut self, command: Command, engine: &SharedEngine) {
        match command {
            Command::Fetch(id) if !self.busy => {
                self.busy = true;
                let outcomes = self.outcomes.clone();
                let version = self.chromium_version.clone();
                std::thread::spawn(move || {
                    let staged = web_store::download(&id, &version)
                        .and_then(|bytes| extensions::stage_package(bytes, &id))
                        .map_err(|error| error.to_string());
                    if let Ok(mut queue) = outcomes.lock() {
                        queue.push(Outcome::Staged(staged));
                    }
                });
            }
            Command::StageFile(path) if !self.busy => {
                self.busy = true;
                let outcomes = self.outcomes.clone();
                std::thread::spawn(move || {
                    let staged = read_package_file(&path).and_then(|bytes| {
                        extensions::stage_file(bytes).map_err(|error| error.to_string())
                    });
                    if let Ok(mut queue) = outcomes.lock() {
                        queue.push(Outcome::Staged(staged));
                    }
                });
            }
            Command::Confirm => {
                if let Some(Panel::Offer(package)) = self.panel.take() {
                    self.busy = true;
                    let outcomes = self.outcomes.clone();
                    std::thread::spawn(move || {
                        let root = extensions::extensions_dir();
                        let installed = std::fs::create_dir_all(&root)
                            .map_err(|error| error.to_string())
                            .and_then(|()| {
                                package.install(&root).map_err(|error| error.to_string())
                            });
                        if let Ok(mut queue) = outcomes.lock() {
                            queue.push(Outcome::Installed(installed));
                        }
                    });
                }
            }
            Command::Cancel | Command::ClosePanel => self.panel = None,
            Command::Enable(id) => self.set_enabled(&id, true),
            Command::Disable(id) => self.set_enabled(&id, false),
            Command::Remove(id) => {
                if let Some(info) = self.info_of(&id).cloned() {
                    self.host
                        .reconcile(&[], std::slice::from_ref(&id), ui::time::now());
                    self.host.forget(&id);
                    self.change_profile(move || {
                        extensions::uninstall(&info, &extensions::extensions_dir())
                            .map_err(|error| error.to_string())
                    });
                }
            }
            Command::Pin(id, pinned) => {
                self.change_profile(move || {
                    extensions::set_pinned(&id, pinned).map_err(|error| error.to_string())
                });
            }
            Command::OpenAction(id) => self.open_action(&id, engine),
            // Busy: ignore a second fetch until the first settles.
            Command::Fetch(_) | Command::StageFile(_) => {}
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(info) = self.info_of(id).cloned() {
            self.change_profile(move || {
                extensions::set_approved(&info, enabled).map_err(|error| error.to_string())
            });
        }
    }

    fn open_action(&mut self, id: &str, engine: &SharedEngine) {
        let Some(url) = self
            .installed
            .iter()
            .find(|extension| extension.id == id)
            .and_then(Installed::popup_url)
        else {
            self.panel = Some(Panel::Message(
                "This extension has no popup to show.".into(),
            ));
            return;
        };
        if matches!(&self.panel, Some(Panel::Popup { id: open, .. }) if open == id) {
            self.panel = None;
            return;
        }
        match engine.create_tab() {
            Ok(popup) => {
                popup.send(WebViewCommand::ReportDocumentSize(true));
                popup.send(WebViewCommand::Navigate(url));
                self.panel = Some(Panel::Popup {
                    id: id.to_owned(),
                    engine: popup,
                    size: None,
                });
            }
            Err(error) => self.panel = Some(Panel::Message(error)),
        }
    }

    fn model(&self) -> toolbar::Model {
        toolbar::model(&self.installed, |id| self.host.action_state(id))
    }

    /// The buttons this controller adds to the toolbar row, in order: the
    /// Web Store offer, pinned actions, and the Extensions menu.
    pub fn toolbar_buttons(&self) -> Vec<Element> {
        if !self.active {
            return Vec::new();
        }
        let mut buttons: Vec<Element> = Vec::new();
        if let Some(item) = &self.store_item
            && !self
                .installed
                .iter()
                .any(|extension| extension.id == item.id)
        {
            let label = if self.busy {
                "Adding\u{2026}"
            } else {
                "Add to Tilo Browser"
            };
            buttons.push(Box::new(
                Button::new(ADD_BUTTON, label)
                    .with_icon(Icon::Download)
                    .with_enabled(!self.busy)
                    .on_effect(self.queue(Command::Fetch(item.id.clone()))),
            ));
        }
        let model = self.model();
        for entry in &model.pinned {
            let mut button = Button::icon_only(
                &format!("browser.extension.action.{}", entry.id),
                Icon::Box,
                &entry.label,
            )
            .tooltip(&entry.tooltip)
            .on_effect(self.queue(Command::OpenAction(entry.id.clone())));
            if let Some(Some(image)) = self.icons.get(&entry.id) {
                button = button.with_image(image.clone(), Some(&entry.badge));
            }
            buttons.push(Box::new(button));
        }
        if !model.menu.is_empty() {
            buttons.push(Box::new(self.extensions_menu(&model)));
        }
        buttons
    }

    /// The Web Store offer as a slot of the window's bar, which stands for
    /// the toolbar row in a Tilo session: on a store item not yet added.
    pub fn bar_offer(&self) -> Option<ToolbarAction> {
        if !self.active {
            return None;
        }
        let item = self.store_item.as_ref()?;
        if self
            .installed
            .iter()
            .any(|extension| extension.id == item.id)
        {
            return None;
        }
        let name = CATALOG.t(if self.busy {
            "browser-extension-adding"
        } else {
            "browser-extension-add"
        });
        Some(ToolbarAction::new(BAR_ADD, "download", name).enabled(!self.busy))
    }

    /// The bar's Extensions items: one per enabled extension with an action,
    /// which opens its popup, as the Extensions menu's rows do.
    pub fn bar_items(&self) -> Vec<ToolbarAction> {
        if !self.active {
            return Vec::new();
        }
        self.model()
            .menu
            .into_iter()
            .map(|entry| {
                let name = if entry.button.badge.is_empty() {
                    entry.button.label
                } else {
                    format!("{} ({})", entry.button.label, entry.button.badge)
                };
                ToolbarAction::new(format!("{BAR_OPEN}{}", entry.button.id), "box", name)
            })
            .collect()
    }

    /// Run a bar action from [`Self::bar_offer`] or [`Self::bar_items`] on
    /// the next `sync`. Returns whether `id` was one of them.
    pub fn bar_action(&mut self, id: &str) -> bool {
        let command = if id == BAR_ADD {
            match &self.store_item {
                Some(item) => Command::Fetch(item.id.clone()),
                None => return true,
            }
        } else if let Some(extension) = id.strip_prefix(BAR_OPEN) {
            Command::OpenAction(extension.to_owned())
        } else {
            return false;
        };
        (self.queue(command))();
        true
    }

    fn extensions_menu(&self, model: &toolbar::Model) -> MenuButton {
        let mut items = Vec::new();
        for entry in &model.menu {
            let open = self.queue(Command::OpenAction(entry.button.id.clone()));
            let open = std::cell::RefCell::new(open);
            let label = if entry.button.badge.is_empty() {
                entry.button.label.clone()
            } else {
                format!("{} ({})", entry.button.label, entry.button.badge)
            };
            items.push(MenuItem::action(&label, move || (open.borrow_mut())()));
            let pin = self.queue(Command::Pin(entry.button.id.clone(), !entry.pinned));
            let pin = std::cell::RefCell::new(pin);
            items.push(
                MenuItem::action(
                    format!("Pin {} to the toolbar", entry.button.label),
                    move || (pin.borrow_mut())(),
                )
                .with_checked(entry.pinned),
            );
        }
        Button::icon_only(MENU_BUTTON, Icon::Box, "Extensions")
            .tooltip("Extensions")
            .with_menu(items)
            .aligned(PanelAlign::Trailing)
    }

    /// The anchored panel: what to show, and the controls to open beside,
    /// nearest first.
    pub fn panel(&self, width: f32, max_height: f32) -> Option<(Element, Vec<String>)> {
        let panel = self.panel.as_ref()?;
        let text = |line: &str| -> Element {
            Box::new(Text::new(line).with_role(ui::tokens::TextRole::Body))
        };
        Some(match panel {
            Panel::Offer(package) => {
                let mut content = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(ui::tokens::Rhythm::Sibling)
                    .push(Box::new(
                        Text::new(&format!("Add {} to Tilo Browser?", package.info.name))
                            .with_role(ui::tokens::TextRole::Heading),
                    ))
                    .push(text(&format!("Version {}", package.info.version)))
                    .push(Box::new(
                        Text::new(&CATALOG.t("browser-extension-can"))
                            .with_role(ui::tokens::TextRole::Body),
                    ));
                // What it may do, one line each, as Chrome lists it.
                for line in &package.permissions {
                    content = content.push(Box::new(
                        Text::new(line)
                            .with_role(ui::tokens::TextRole::Caption)
                            .dimmed(),
                    ));
                }
                let content = content.push(Box::new(
                    Actions::new(
                        Button::new("browser.extension.confirm", "Add extension")
                            .on_effect(self.queue(Command::Confirm)),
                    )
                    .secondary(
                        Button::new("browser.extension.cancel", "Cancel")
                            .on_effect(self.queue(Command::Cancel)),
                    ),
                ));
                (
                    Box::new(content),
                    vec![ADD_BUTTON.to_owned(), MENU_BUTTON.to_owned()],
                )
            }
            Panel::Message(message) => (
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_rhythm(ui::tokens::Rhythm::Sibling)
                        .push(text(message))
                        .push(Box::new(
                            Button::new("browser.extension.close", "Close")
                                .on_effect(self.queue(Command::ClosePanel)),
                        )),
                ),
                vec![ADD_BUTTON.to_owned(), MENU_BUTTON.to_owned()],
            ),
            Panel::Popup { id, engine, size } => {
                let view = ui::widget::WebView::new(engine.clone())
                    .with_test_id("browser.extension.popup");
                let page = size.unwrap_or(POPUP_SIZE);
                let size = (page.0.min(width), page.1.min(max_height));
                (
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(size.0)
                            .with_height(size.1)
                            .with_child(Box::new(view)),
                    ),
                    vec![
                        format!("browser.extension.action.{id}"),
                        MENU_BUTTON.to_owned(),
                    ],
                )
            }
        })
    }

    /// The Library page's Extensions section: an install-from-file row, the
    /// notice, and one row per installed extension.
    pub fn library_rows(&self, matches: impl Fn(&[&str]) -> bool) -> Vec<Element> {
        let mut rows: Vec<Element> = Vec::new();
        for (info, extension) in self.infos.iter().zip(&self.installed) {
            if !matches(&[&info.name, &extension.id, &info.version]) {
                continue;
            }
            rows.push(self.library_row(info, extension));
        }
        rows
    }

    fn library_row(&self, info: &ExtensionInfo, extension: &Installed) -> Element {
        let id = &extension.id;
        let running = self.host.is_loaded(id);
        let status = if info.manifest_version != 3 {
            "Not supported: only Manifest V3 extensions run"
        } else if running {
            "Running"
        } else if extension.enabled {
            "Starting\u{2026}"
        } else {
            "Off"
        };
        // Its broadest access, the line Chrome lists first.
        let permissions = extensions::permission_summary(info).map_or_else(
            |e| e.to_string(),
            |lines| lines.into_iter().next().unwrap_or_default(),
        );
        let mut buttons = Vec::new();
        if info.manifest_version == 3 {
            let command = if extension.enabled {
                Command::Disable(id.clone())
            } else {
                Command::Enable(id.clone())
            };
            buttons.push(
                Button::new(
                    &format!("browser.extension.toggle.{id}"),
                    if extension.enabled {
                        "Turn off"
                    } else {
                        "Turn on"
                    },
                )
                .on_effect(self.queue(command)),
            );
        }
        if extension.enabled && extension.action.is_some() {
            buttons.push(
                Button::new(
                    &format!("browser.extension.pin.{id}"),
                    if extension.pinned { "Unpin" } else { "Pin" },
                )
                .on_effect(self.queue(Command::Pin(id.clone(), !extension.pinned))),
            );
        }
        buttons.push(
            Button::new(&format!("browser.extension.remove.{id}"), "Remove")
                .on_effect(self.queue(Command::Remove(id.clone()))),
        );
        Box::new(crate::rows::with_buttons(
            ListRow::new(format!("browser.extension.{id}"), info.name.clone())
                .secondary(format!("{status}. {permissions}"))
                .two_line_secondary()
                .meta(info.version.clone()),
            buttons,
        ))
    }

    /// The header of the Extensions section: how to add one.
    pub fn library_header(&self) -> Element {
        let file_path = self.file_path.clone();
        let install = {
            let commands = self.commands.clone();
            let path = self.file_path.clone();
            move || {
                let value = path.get();
                if value.trim().is_empty() {
                    return;
                }
                if let Ok(mut queue) = commands.lock() {
                    queue.push(Command::StageFile(PathBuf::from(value.trim())));
                }
            }
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Sibling)
                .push(Box::new(
                    Text::new(
                        "Open an extension on the Chrome Web Store and choose Add to Tilo Browser, \
                         or install a downloaded .crx or .zip.",
                    )
                    .with_role(ui::tokens::TextRole::Caption)
                    .dimmed(),
                ))
                .push(Box::new(
                    ui::widget::Row::new()
                        .no_wrap()
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_flex_grow()
                                .with_child(Box::new(
                                    TextInput::from_state(&file_path)
                                        .with_id("browser.extension.file")
                                        .with_accessible_name("Extension package file")
                                        .with_placeholder("Path to a .crx or .zip file"),
                                )),
                        ))
                        .push(Box::new(
                            Button::new("browser.extension.install-file", "Install from file")
                                .on_effect(install),
                        )),
                )),
        )
    }

    pub fn notice(&self) -> Option<Element> {
        match &self.panel {
            Some(Panel::Message(message)) => Some(Box::new(Banner::info(message))),
            _ => None,
        }
    }
}

fn read_package_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    const LIMIT: u64 = 64 * 1024 * 1024;
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("the file is larger than 64 MiB".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_ITEM: &str = "https://chromewebstore.google.com/detail/ublock-origin-lite/ddkjiahejlhfcafbddmgiahcphecmpfh";

    fn queued(controller: &ExtensionController) -> Vec<String> {
        controller
            .commands
            .lock()
            .unwrap()
            .iter()
            .map(|command| format!("{command:?}"))
            .collect()
    }

    #[test]
    fn the_window_bar_offers_a_store_item_and_opens_extension_popups() {
        let mut controller = ExtensionController::new(true);
        assert_eq!(controller.bar_offer(), None, "no offer away from the store");
        controller.store_item = web_store::item_from_url(STORE_ITEM);
        let offer = controller.bar_offer().expect("a store item is offered");
        assert_eq!(offer.id, BAR_ADD);
        assert!(offer.enabled);
        assert!(controller.bar_action(BAR_ADD));
        assert_eq!(
            queued(&controller),
            [r#"Fetch("ddkjiahejlhfcafbddmgiahcphecmpfh")"#]
        );
        controller.busy = true;
        assert!(
            !controller.bar_offer().unwrap().enabled,
            "one download at a time"
        );

        assert!(controller.bar_action(&format!("{BAR_OPEN}abc")));
        assert_eq!(queued(&controller)[1], r#"OpenAction("abc")"#);
        assert!(!controller.bar_action("reload"), "not this controller's");

        let private = ExtensionController::new(false);
        assert_eq!(private.bar_offer(), None);
        assert!(private.bar_items().is_empty());
    }

    #[test]
    fn a_popup_is_as_big_as_its_page_and_closes_when_its_page_closes_it() {
        let window: SharedEngine = ui::webview::test_engine::TestEngine::new();
        let page = ui::webview::test_engine::TestEngine::new();
        let mut controller = ExtensionController::new(true);
        controller.refresh = false;
        controller.panel = Some(Panel::Popup {
            id: "abc".into(),
            engine: page.clone(),
            size: None,
        });
        let shown = |controller: &ExtensionController| match &controller.panel {
            Some(Panel::Popup { size, .. }) => *size,
            _ => None,
        };
        page.push_event(ui::webview::WebViewEvent::DocumentSize {
            width: 380.0,
            height: 430.0,
            scroll_y: 0.0,
        });
        assert!(controller.sync(&window, ""));
        assert_eq!(shown(&controller), Some((380.0, 430.0)));
        // Never past Chrome's largest popup.
        page.push_event(ui::webview::WebViewEvent::DocumentSize {
            width: 2000.0,
            height: 2000.0,
            scroll_y: 0.0,
        });
        controller.sync(&window, "");
        assert_eq!(shown(&controller), Some(POPUP_MAX));
        // Its page called window.close(): the panel goes.
        page.push_event(ui::webview::WebViewEvent::TabClosed);
        assert!(controller.sync(&window, ""));
        assert!(controller.panel.is_none());
    }

    #[test]
    fn a_tilo_windows_bar_shows_every_extensions_badge() {
        let extension = |id: &str, pinned: bool, action: bool| Installed {
            id: id.into(),
            name: id.into(),
            root: std::path::PathBuf::new(),
            enabled: true,
            pinned,
            action: action.then(crate::extension_toolbar::ActionManifest::default),
        };
        let installed = [
            extension("pinned", true, true),
            extension("listed", false, true),
            extension("actionless", false, false),
        ];
        assert_eq!(badges_shown(&installed, true), ["pinned", "listed"]);
        assert_eq!(badges_shown(&installed, false), ["pinned"]);
    }
}
