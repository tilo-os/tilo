//! Extensions in the browser window: the Web Store install offer, the
//! toolbar's action buttons and popups, and the Library page's manager.
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
    Actions, Banner, Button, Column, Container, Element, ListRow, MenuButton, MenuItem, Text,
    TextInput,
};

use crate::extension_host::{ExtensionHost, Wanted};
use crate::extension_toolbar::{self as toolbar, Installed};
use crate::extensions::{self, ExtensionInfo, StagedPackage};
use crate::web_store;

/// Width and height of an extension popup, in logical pixels.
const POPUP_SIZE: (f32, f32) = (360.0, 560.0);

const ADD_BUTTON: &str = "browser.extension.add";
const MENU_BUTTON: &str = "browser.extensions.menu";

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

/// A slow step's result.
enum Outcome {
    Staged(Result<StagedPackage, String>),
    Installed(Result<ExtensionInfo, String>),
}

/// What the anchored panel shows.
enum Panel {
    Offer(Box<StagedPackage>),
    Message(String),
    Popup { id: String, engine: SharedEngine },
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

    /// A reply from the engine to one of this controller's DevTools calls.
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
                Outcome::Staged(Err(error)) => {
                    self.panel = Some(Panel::Message(format!(
                        "Could not add the extension: {error}"
                    )));
                }
                Outcome::Installed(Ok(info)) => {
                    if let Err(error) = extensions::set_approved(&info, true) {
                        self.panel = Some(Panel::Message(error.to_string()));
                    } else {
                        self.panel = Some(Panel::Message(format!("{} was added", info.name)));
                    }
                }
                Outcome::Installed(Err(error)) => {
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
            Command::OpenAction(id) => self.open_action(&id, engine),
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
                    &format!("Pin {} to the toolbar", entry.button.label),
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
                        Text::new(&format!("Add {} to Tilo Browser?", package.info.name))
                            .with_role(ui::tokens::TextRole::Heading),
                    ))
                    .push(text(&format!("Version {}", package.info.version)))
                    .push(Box::new(
                        Text::new(&format!("It can use: {}", package.permissions))
                            .with_role(ui::tokens::TextRole::Caption)
                            .dimmed(),
                    ))
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
        let permissions = extensions::permission_summary(info).unwrap_or_else(|e| e.to_string());
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
