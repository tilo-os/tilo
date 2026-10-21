//! Extensions in the browser window: the Web Store install offer, the
//! action buttons and their popups, the Extensions panel, and the settings'
//! Extensions section.
//!
//! In a Tilo session the toolbar is the window bar's slots: pinned actions
//! are bar actions carrying the extension's icon and badge
//! ([`ExtensionController::bar_actions`]), and what one opens hangs under
//! the button it was pressed on. Outside a session the window's own toolbar
//! row carries the same buttons ([`ExtensionController::toolbar_buttons`]).
//!
//! The controller owns no widget state the document needs; it is the
//! window's local, device-only view of the profile's extensions. Interface
//! closures queue [`Command`]s, `sync` runs them and starts the slow parts
//! (download, verify, unpack) on worker threads whose results come back
//! through [`Outcome`]s, so nothing here blocks a frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ui::icons::Icon;
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::floating_surface::PanelAlign;
use ui::widget::{
    Actions, Banner, BannerKind, Button, ButtonRole, Card, Column, Container, Element, ListRow,
    MenuButton, MenuItem, Text, TextInput, Toggle,
};

use crate::extension_host::{ExtensionHost, Wanted};
use crate::extension_toolbar::{self as toolbar, Installed};
use crate::extensions::{self, ExtensionInfo, StagedPackage};
use crate::web_store;

/// Width and height of an extension popup, in logical pixels.
const POPUP_SIZE: (f32, f32) = (360.0, 560.0);

const ADD_BUTTON: &str = "browser.extension.add";
const MENU_BUTTON: &str = "browser.extensions.menu";

/// The largest action icon handed to the window's bar, in bytes.
const MAX_BAR_ICON_BYTES: u64 = 64 * 1024;

/// Something the person asked of the window from an extension control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowRequest {
    /// Open the settings' Extensions section.
    ManageExtensions,
}

/// A pinned extension's action as the window's bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarAction {
    /// The extension's Chromium identifier.
    pub id: String,
    /// What the button is called: the extension's name, and its action's
    /// title when that says more.
    pub name: String,
    pub badge: String,
    /// The action's icon, as the PNG the extension ships.
    pub png: Option<Vec<u8>>,
}

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
    /// Show every extension, to open, pin or manage one.
    OpenMenu,
    /// Go to the settings' Extensions section.
    Manage,
    ClosePanel,
}

/// A slow step's result.
enum Outcome {
    Staged(Result<StagedPackage, String>),
    Installed(Result<ExtensionInfo, String>),
}

/// What the anchored panel shows.
enum Panel {
    Offer(Box<StagedPackage>),
    Message(String),
    Popup {
        id: String,
        engine: SharedEngine,
    },
    /// The Extensions panel: every enabled extension with an action.
    Menu,
}

pub struct ExtensionController {
    /// Whether this window manages extensions: not a private or headless one.
    active: bool,
    commands: Arc<Mutex<Vec<Command>>>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    host: ExtensionHost,
    installed: Vec<Installed>,
    infos: Vec<ExtensionInfo>,
    icons: HashMap<String, Option<vello::peniko::ImageData>>,
    /// The action icons as the files the extensions ship, for the bar.
    icon_files: HashMap<String, Option<Vec<u8>>>,
    /// What the person asked of the window (to manage extensions: it opens
    /// its settings), until the window takes it.
    window_request: Option<WindowRequest>,
    /// Screenshots only: the fixture's extensions that count as running,
    /// with no engine to load them.
    running_for_shots: Vec<String>,
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
            host: ExtensionHost::new(),
            installed: Vec::new(),
            infos: Vec::new(),
            icons: HashMap::new(),
            icon_files: HashMap::new(),
            window_request: None,
            running_for_shots: Vec::new(),
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

    /// Whether this window manages extensions (not a private one).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Open extension `id`'s action: its popup, under its button.
    pub fn open_action(&self, id: &str) {
        (self.queue(Command::OpenAction(id.to_owned())))();
    }

    /// Show or hide the Extensions panel.
    pub fn toggle_menu(&self) {
        (self.queue(Command::OpenMenu))();
    }

    /// Whether the person asked to manage extensions since the last call.
    pub fn take_manage_request(&mut self) -> bool {
        self.window_request.take() == Some(WindowRequest::ManageExtensions)
    }

    /// Whether any extension is installed: the Extensions control shows.
    pub fn has_extensions(&self) -> bool {
        self.active && !self.installed.is_empty()
    }

    /// The pinned extensions' actions, for the window's bar, in order.
    pub fn bar_actions(&self) -> Vec<BarAction> {
        if !self.active {
            return Vec::new();
        }
        self.model()
            .pinned
            .into_iter()
            .map(|button| BarAction {
                png: self.icon_files.get(&button.id).cloned().flatten(),
                name: button.tooltip,
                badge: button.badge,
                id: button.id,
            })
            .collect()
    }

    /// On a Chrome Web Store extension page not added yet: the offer to add
    /// it, under the toolbar. `page_title` names it until its package is
    /// read.
    pub fn store_notice(&self, page_title: &str) -> Option<Element> {
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
        let name = page_title
            .trim()
            .trim_end_matches("Chrome Web Store")
            .trim_end()
            .trim_end_matches(['-', '\u{2013}', '\u{2014}'])
            .trim();
        let title = if name.is_empty() {
            "Add this extension to Browser".to_owned()
        } else {
            format!("Add {name} to Browser")
        };
        let button = Button::new(
            ADD_BUTTON,
            if self.busy {
                "Adding\u{2026}"
            } else {
                "Add to Browser"
            },
        )
        .with_icon(Icon::Download)
        .with_enabled(!self.busy)
        .on_effect(self.queue(Command::Fetch(item.id.clone())));
        Some(Box::new(
            Container::new().with_child(Box::new(
                Banner::new(BannerKind::Neutral, title)
                    .with_message("You see what it can do before it is added.")
                    .with_id("browser.extension.offer")
                    .with_action(button),
            )),
        ))
    }

    /// The settings' Extensions section, as the settings page builds it.
    pub fn settings_section(&self) -> Option<ExtensionsSection> {
        if !self.active {
            return None;
        }
        let entries = self
            .infos
            .iter()
            .zip(&self.installed)
            .map(|(info, extension)| SectionEntry {
                id: extension.id.clone(),
                name: info.name.clone(),
                version: info.version.clone(),
                state: if info.manifest_version != 3 {
                    EntryState::Unsupported
                } else if !extension.enabled {
                    EntryState::Off
                } else if self.running_for_shots.contains(&extension.id)
                    || self.host.is_loaded(&extension.id)
                {
                    EntryState::On
                } else {
                    EntryState::Starting
                },
                pinned: extension.pinned,
                has_action: extension.action.is_some(),
                icon: self.icons.get(&extension.id).cloned().flatten(),
                permissions: extensions::permission_warnings_of(info)
                    .unwrap_or_else(|error| vec![error.to_string()]),
            })
            .collect();
        Some(ExtensionsSection {
            entries,
            commands: self.commands.clone(),
            file_path: self.file_path.clone(),
            message: match &self.panel {
                Some(Panel::Message(message)) => Some(message.clone()),
                _ => None,
            },
        })
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
            self.busy = false;
            self.refresh = true;
            changed = true;
            match outcome {
                Outcome::Staged(Ok(package)) => self.panel = Some(Panel::Offer(Box::new(package))),
                Outcome::Installed(Ok(info)) => {
                    if let Err(error) = extensions::set_approved(&info, true) {
                        self.panel = Some(Panel::Message(error.to_string()));
                    } else {
                        self.panel = Some(Panel::Message(format!("{} was added", info.name)));
                    }
                }
                Outcome::Staged(Err(error)) | Outcome::Installed(Err(error)) => {
                    self.panel = Some(Panel::Message(format!(
                        "Could not add the extension: {error}"
                    )));
                }
            }
        }
        if self.refresh {
            self.refresh = false;
            self.reload();
            changed = true;
        }
        let now = ui::time::now();
        let pinned: Vec<String> = self
            .installed
            .iter()
            .filter(|extension| extension.enabled && extension.pinned)
            .map(|extension| extension.id.clone())
            .collect();
        self.host.probe(&pinned, now);
        self.host.flush(engine);
        if let Some(Panel::Popup { engine: popup, .. }) = &self.panel {
            // The popup is a tab of its own: its page events are not the
            // window's, but they must not pile up.
            let _ = popup.drain_events();
            let _ = popup.take_new_tab_requests();
        }
        changed
    }

    /// Re-read the profile and bring the engine in line with it.
    fn reload(&mut self) {
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
        let wanted: Vec<Wanted> = installed
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
        let unwanted: Vec<String> = installed
            .iter()
            .filter(|extension| !extension.enabled)
            .map(|extension| extension.id.clone())
            .collect();
        self.host.reconcile(&wanted, &unwanted, ui::time::now());
        self.icons
            .retain(|id, _| installed.iter().any(|e| &e.id == id));
        self.icon_files
            .retain(|id, _| installed.iter().any(|e| &e.id == id));
        for extension in &installed {
            self.icon_files
                .entry(extension.id.clone())
                .or_insert_with(|| extension.icon_path().and_then(|path| png_file(&path)));
        }
        for extension in &installed {
            self.icons.entry(extension.id.clone()).or_insert_with(|| {
                extension
                    .icon_path()
                    .and_then(|path| toolbar::read_icon(&path))
            });
        }
        self.installed = installed;
        self.infos = infos;
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
                    let result = extensions::uninstall(&info, &extensions::extensions_dir());
                    self.host
                        .reconcile(&[], std::slice::from_ref(&id), ui::time::now());
                    self.host.forget(&id);
                    if let Err(error) = result {
                        self.panel = Some(Panel::Message(error.to_string()));
                    }
                    self.refresh = true;
                }
            }
            Command::Pin(id, pinned) => {
                if let Err(error) = extensions::set_pinned(&id, pinned) {
                    self.panel = Some(Panel::Message(error.to_string()));
                }
                self.refresh = true;
            }
            Command::OpenAction(id) => self.start_action(&id, engine),
            Command::OpenMenu => {
                self.panel = if matches!(self.panel, Some(Panel::Menu)) {
                    None
                } else {
                    Some(Panel::Menu)
                };
            }
            Command::Manage => {
                self.panel = None;
                self.window_request = Some(WindowRequest::ManageExtensions);
            }
            // Busy: ignore a second fetch until the first settles.
            Command::Fetch(_) | Command::StageFile(_) => {}
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(info) = self.info_of(id).cloned() {
            if let Err(error) = extensions::set_approved(&info, enabled) {
                self.panel = Some(Panel::Message(error.to_string()));
            }
            self.refresh = true;
        }
    }

    fn start_action(&mut self, id: &str, engine: &SharedEngine) {
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
                popup.send(WebViewCommand::Navigate(url));
                self.panel = Some(Panel::Popup {
                    id: id.to_owned(),
                    engine: popup,
                });
            }
            Err(error) => self.panel = Some(Panel::Message(error)),
        }
    }

    // ---- Interface ------------------------------------------------------

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
    pub fn panel(&self, width: f32) -> Option<(Element, Vec<String>)> {
        let panel = self.panel.as_ref()?;
        let text = |line: &str| -> Element {
            Box::new(Text::new(line).with_role(ui::tokens::TextRole::Body))
        };
        Some(match panel {
            Panel::Offer(package) => {
                let content = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(ui::tokens::Rhythm::Sibling)
                    .push(Box::new(
                        Text::new(&format!("Add {} to Browser?", package.info.name))
                            .with_role(ui::tokens::TextRole::Heading),
                    ))
                    .push(Box::new(
                        Text::new(&format!("Version {}", package.info.version))
                            .with_role(ui::tokens::TextRole::Caption)
                            .dimmed(),
                    ))
                    .push(Box::new(
                        Text::new("It can:").with_role(ui::tokens::TextRole::Body),
                    ))
                    .push(Box::new(permission_list(&package.permissions)))
                    .push(Box::new(
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
            Panel::Menu => (self.menu_panel(), vec![MENU_BUTTON.to_owned()]),
            Panel::Popup { id, engine } => {
                let view = ui::widget::WebView::new(engine.clone())
                    .with_test_id("browser.extension.popup");
                let size = (POPUP_SIZE.0.min(width), POPUP_SIZE.1);
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

    /// Screenshots: three extensions unpacked under `root` (two on and
    /// pinned, one off), with `panel` open: `"menu"` the Extensions panel,
    /// `"offer"` a Web Store page's offer and the permissions to confirm.
    /// Nothing is read from or written to the profile.
    pub fn stage_for_shots(&mut self, root: &std::path::Path, panel: &str) {
        let fixtures = [
            (
                "ublock-origin-lite",
                "uBlock Origin Lite",
                "2026.10.12",
                r#""permissions":["declarativeNetRequest","storage"]"#,
                [196, 0, 0],
                true,
            ),
            (
                "bitwarden",
                "Bitwarden Password Manager",
                "2026.9.1",
                r#""permissions":["tabs","storage","clipboardWrite","contextMenus"],"host_permissions":["<all_urls>"]"#,
                [23, 93, 220],
                true,
            ),
            (
                "dark-reader",
                "Dark Reader",
                "4.9.110",
                r#""permissions":["storage"],"host_permissions":["*://*/*"]"#,
                [30, 30, 30],
                false,
            ),
        ];
        let mut infos = Vec::new();
        let mut installed = Vec::new();
        for (dir, name, version, permissions, colour, on) in fixtures {
            let path = root.join(dir);
            let _ = std::fs::create_dir_all(&path);
            let manifest = format!(
                r#"{{"manifest_version":3,"name":"{name}","version":"{version}",{permissions},"action":{{"default_popup":"popup.html","default_icon":{{"32":"icon.png"}}}}}}"#
            );
            let _ = std::fs::write(path.join("manifest.json"), manifest);
            let _ = std::fs::write(path.join("popup.html"), "<title>popup</title>");
            if let Ok(png) = ui::png::encode_rgba8(32, 32, &shot_icon(colour)) {
                let _ = std::fs::write(path.join("icon.png"), png);
            }
            let Some(info) = extensions::list_in(root)
                .into_iter()
                .find(|info| info.path.ends_with(dir))
            else {
                continue;
            };
            if let Some(extension) = Installed::read(&info, on, on) {
                installed.push(extension);
                infos.push(info);
            }
        }
        self.active = true;
        self.refresh = false;
        self.running_for_shots = installed
            .iter()
            .filter(|extension| extension.enabled)
            .map(|extension| extension.id.clone())
            .collect();
        self.installed = installed;
        self.infos = infos;
        self.icons.clear();
        self.icon_files.clear();
        for extension in &self.installed {
            let path = extension.icon_path();
            self.icons.insert(
                extension.id.clone(),
                path.as_deref().and_then(toolbar::read_icon),
            );
            self.icon_files
                .insert(extension.id.clone(), path.as_deref().and_then(png_file));
        }
        match panel {
            "menu" => self.panel = Some(Panel::Menu),
            "offer" => {
                let manifest = r#"{"manifest_version":3,"name":"Grammar Checker","version":"3.2.0","permissions":["storage","contextMenus"],"host_permissions":["<all_urls>"]}"#;
                self.store_item = Some(web_store::Item { id: "a".repeat(32) });
                self.panel = Some(Panel::Offer(Box::new(StagedPackage {
                    info: ExtensionInfo {
                        id: "a".repeat(32),
                        name: "Grammar Checker".into(),
                        version: "3.2.0".into(),
                        manifest_version: 3,
                        description: String::new(),
                        permissions: vec!["storage".into(), "contextMenus".into()],
                        path: root.join("grammar-checker"),
                    },
                    permissions: extensions::permission_warnings(manifest).unwrap_or_default(),
                    bytes: Arc::new(Vec::new()),
                    signed_id: None,
                })));
            }
            _ => self.panel = None,
        }
    }

    /// The Extensions panel: each enabled extension with an action, opened
    /// by a press on its row, with its pin; and the way to manage them.
    fn menu_panel(&self) -> Element {
        let model = self.model();
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Sibling)
            .push(Box::new(
                Text::new("Extensions")
                    .with_role(ui::tokens::TextRole::Heading)
                    .with_heading_level(2),
            ));
        if model.menu.is_empty() {
            column = column.push(Box::new(
                Text::new("None of your extensions has a button.")
                    .with_role(ui::tokens::TextRole::Body)
                    .dimmed(),
            ));
        }
        for entry in &model.menu {
            let id = &entry.button.id;
            let mut row = ListRow::new(
                format!("browser.extension.menu.{id}"),
                entry.button.label.clone(),
            )
            .on_activate(self.queue(Command::OpenAction(id.clone())));
            if !entry.button.badge.is_empty() {
                row = row.meta(entry.button.badge.clone());
            }
            row = match self.icons.get(id) {
                Some(Some(image)) => row.thumbnail(image.clone()),
                _ => row.icon(Icon::Box),
            };
            let pin = Button::icon_only(
                &format!("browser.extension.menu-pin.{id}"),
                Icon::Pin,
                &format!("Pin {} to the toolbar", entry.button.label),
            )
            .toggled(entry.pinned)
            .on_effect(self.queue(Command::Pin(id.clone(), !entry.pinned)));
            column = column.push(Box::new(row.trailing(Box::new(pin))));
        }
        Box::new(
            column.push(Box::new(
                Button::new("browser.extensions.manage", "Manage extensions")
                    .with_icon(Icon::Settings)
                    .role(ButtonRole::Plain)
                    .on_effect(self.queue(Command::Manage)),
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

/// The PNG at `path`, when it is one and small enough for the window's bar.
fn png_file(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_BAR_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_BAR_ICON_BYTES && bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
        .then_some(bytes)
}

/// What an extension may do, one line each.
fn permission_list(lines: &[String]) -> Column {
    lines.iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight),
        |column, line| {
            column.push(Box::new(
                ui::widget::Row::new()
                    .no_wrap()
                    .push(Box::new(ui::widget::IconWidget::new(Icon::Check).dim()))
                    .push(Box::new(
                        Text::new(line).with_role(ui::tokens::TextRole::Body),
                    )),
            ))
        },
    )
}

/// Whether an extension in the settings runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryState {
    /// Not a Manifest V3 extension: Browser cannot run it.
    Unsupported,
    Off,
    /// On, and the engine is still loading it.
    Starting,
    On,
}

impl EntryState {
    fn enabled(self) -> bool {
        matches!(self, Self::Starting | Self::On)
    }
}

/// One extension in the settings' Extensions section.
#[derive(Clone)]
struct SectionEntry {
    id: String,
    name: String,
    version: String,
    state: EntryState,
    pinned: bool,
    has_action: bool,
    icon: Option<vello::peniko::ImageData>,
    permissions: Vec<String>,
}

/// The settings' Extensions section: each installed extension with its
/// switch, what it may do, its pin and its removal; and adding one from a
/// file. It is plain data the settings page builds afresh with every layout.
#[derive(Clone)]
pub struct ExtensionsSection {
    entries: Vec<SectionEntry>,
    commands: Arc<Mutex<Vec<Command>>>,
    file_path: ui::LocalState<String>,
    message: Option<String>,
}

impl ExtensionsSection {
    fn queue(&self, command: Command) -> impl FnMut() + 'static {
        let commands = self.commands.clone();
        move || {
            if let Ok(mut queue) = commands.lock() {
                queue.push(command.clone());
            }
        }
    }

    /// The section's card.
    pub fn card(&self) -> Element {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Sibling);
        if let Some(message) = &self.message {
            body = body.push(Box::new(Banner::info(message)));
        }
        if self.entries.is_empty() {
            body = body.push(Box::new(
                Text::new("Open an extension in the Chrome Web Store and choose Add to Browser.")
                    .with_role(ui::tokens::TextRole::Body)
                    .dimmed(),
            ));
        }
        for entry in &self.entries {
            body = body.push(self.entry(entry));
        }
        body = body.push(self.file_row());
        Box::new(Card::new(Box::new(body)).with_title("Extensions"))
    }

    fn entry(&self, entry: &SectionEntry) -> Element {
        let id = &entry.id;
        let status = match entry.state {
            EntryState::Unsupported => {
                "This kind of extension no longer runs in Browser".to_owned()
            }
            EntryState::Off => "Off".to_owned(),
            EntryState::Starting => "Starting\u{2026}".to_owned(),
            EntryState::On => format!("On, version {}", entry.version),
        };
        let mut row =
            ListRow::new(format!("browser.extension.{id}"), entry.name.clone()).secondary(status);
        row = match &entry.icon {
            Some(image) => row.thumbnail(image.clone()),
            None => row.icon(Icon::Box),
        };
        if entry.state != EntryState::Unsupported {
            let on = ui::LocalState::new(entry.state.enabled());
            let commands = self.commands.clone();
            let id_for_toggle = id.clone();
            row = row.trailing(Box::new(
                Toggle::from_state(&on)
                    .with_id(&format!("browser.extension.toggle.{id}"))
                    .with_accessible_name(&format!("{} on", entry.name))
                    .on_change(move |on| {
                        if let Ok(mut queue) = commands.lock() {
                            queue.push(if on {
                                Command::Enable(id_for_toggle.clone())
                            } else {
                                Command::Disable(id_for_toggle.clone())
                            });
                        }
                    }),
            ));
        }
        let mut buttons = Vec::new();
        if entry.state.enabled() && entry.has_action {
            buttons.push(
                Button::new(
                    &format!("browser.extension.pin.{id}"),
                    if entry.pinned {
                        "Unpin from the toolbar"
                    } else {
                        "Pin to the toolbar"
                    },
                )
                .with_icon(Icon::Pin)
                .on_effect(self.queue(Command::Pin(id.clone(), !entry.pinned))),
            );
        }
        buttons.push(
            Button::new(&format!("browser.extension.remove.{id}"), "Remove")
                .role(ButtonRole::Destructive)
                .on_effect(self.queue(Command::Remove(id.clone()))),
        );
        let mut actions = ui::widget::Row::new().no_wrap();
        for button in buttons {
            actions = actions.push(Box::new(button));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Tight)
                .push(Box::new(row))
                .push(Box::new(permission_list(&entry.permissions)))
                .push(Box::new(actions)),
        )
    }

    /// Adding an extension from a downloaded package.
    fn file_row(&self) -> Element {
        let commands = self.commands.clone();
        let path = self.file_path.clone();
        let install = move || {
            let value = path.get();
            if value.trim().is_empty() {
                return;
            }
            if let Ok(mut queue) = commands.lock() {
                queue.push(Command::StageFile(PathBuf::from(value.trim())));
            }
        };
        Box::new(
            ui::widget::Row::new()
                .no_wrap()
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(
                            TextInput::from_state(&self.file_path)
                                .with_id("browser.extension.file")
                                .with_accessible_name("Extension package file")
                                .with_placeholder("Path to a .crx or .zip file"),
                        )),
                ))
                .push(Box::new(
                    Button::new("browser.extension.install-file", "Add from file")
                        .on_effect(install),
                )),
        )
    }
}

/// A screenshot fixture's icon: a disc in `colour` with a light ring.
fn shot_icon([r, g, b]: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32_i32 {
        for x in 0..32_i32 {
            let (dx, dy) = (x * 2 - 31, y * 2 - 31);
            let d = dx * dx + dy * dy;
            let pixel = if d <= 26 * 26 {
                [r, g, b, 255]
            } else if d <= 30 * 30 {
                [
                    r.saturating_add(60),
                    g.saturating_add(60),
                    b.saturating_add(60),
                    255,
                ]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}
