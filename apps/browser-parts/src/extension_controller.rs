//! Extensions in the browser window: the Web Store install offer, the
//! toolbar's action buttons and popups, and the Library page's manager.
//!
//! The controller owns no widget state the document needs; it is the
//! window's local, device-only view of the profile's extensions. Interface
//! closures queue `Command`s, `sync` runs them and starts the slow parts
//! (download, verify, unpack) on worker threads whose results come back
//! through `Outcome`s, so nothing here blocks a frame. The profile itself
//! (which extensions are installed, approved and pinned, their manifests and
//! icons) is read and written on one worker of its own, in order, since the
//! window may be drawn on the compositor's frame loop.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use ui::icons::Icon;
use ui::webview::{
    ExtensionActionIcon, ExtensionMenuItem, ExtensionPermissionSet, SharedEngine, WebViewCommand,
    WebViewEvent,
};
use ui::widget::floating_surface::PanelAlign;
use ui::widget::{
    Actions, Banner, Button, Column, Container, Element, ListRow, MenuButton, MenuItem, Text,
    TextInput,
};
use ui::window_toolbar::ToolbarAction;

use ui::tilo_actions::{Resource, ResourcePresentation};

use crate::CATALOG;
use crate::capabilities::ExtensionQuestion;
use crate::effects::ExtensionRequest;
use crate::extension_host::{ExtensionHost, Wanted};
use crate::extension_omnibox::ExtensionOmnibox;
use crate::extension_toolbar::{self as toolbar, Installed};
use crate::extensions::{self, ExtensionError, ExtensionInfo, StagedPackage};
use crate::web_store;

mod omnibox;
mod side_panel;
use side_panel::SidePanel;

/// Copy with one name in it, as `key` says it.
fn named(key: &str, name: &str) -> String {
    CATALOG.t_args(key, &[("name", ui::i18n::ArgValue::Text(name.to_owned()))])
}

/// What the person reads when adding an extension failed, with the
/// failure's detail kept for the log.
fn add_failed(error: &ExtensionError) -> String {
    tracing::warn!(%error, "an extension could not be added");
    error.message()
}

/// A refused change to an extension as the person reads it: `key`, with
/// `args`. The failure's detail goes to the log, not the sentence.
fn refusal(error: &ExtensionError, key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    tracing::warn!(%error, key, "a change to an extension failed");
    CATALOG.t_args(key, args)
}

/// An extension popup's size before its page says how big it is, in
/// logical pixels: as wide as a panel, and short, because a page reports at
/// least the view it is laid out in, so a popup can only grow to its page.
const POPUP_SIZE: (f32, f32) = (360.0, 120.0);
/// The largest popup, as in Chrome.
const POPUP_MAX: (f32, f32) = (800.0, 600.0);
/// How wide an extension's side panel beside the page stands in `theme`: a
/// list panel's width, about as wide as Chrome's, which extensions lay their
/// panels out for.
#[must_use]
pub fn side_panel_width(theme: &ui::theme::Theme) -> f32 {
    theme.tokens.metrics.panel_width_list
}

const ADD_BUTTON: &str = "browser.extension.add";
const MENU_BUTTON: &str = "browser.extensions.menu";

/// The window bar's action ids: the Web Store offer, the prefix of an item
/// opening one extension's popup, and of an item of one extension's button
/// menu (followed by the extension's id and the item's key).
const BAR_ADD: &str = "extension-add";
const BAR_OPEN: &str = "extension-open:";
const BAR_MENU: &str = "extension-menu:";
/// Chrome's extension ids: 32 letters.
const EXTENSION_ID_LEN: usize = 32;
/// The prefix of an item showing or hiding one extension's side panel.
const BAR_SIDE_PANEL: &str = "extension-side-panel:";

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
    /// Take back what the person allowed the extension after it was added.
    Revoke(String),
    Pin(String, bool),
    OpenAction(String),
    /// Show an extension's settings page.
    OpenSettings(String),
    /// Words for an extension's keyword in the address, as Enter sends them.
    Send(String, String),
    ClosePanel,
    /// Choose an item of an extension's button menu: (extension, key).
    ChooseMenuItem(String, String),
    /// Show an extension's side panel beside the page, or close it.
    OpenSidePanel(String),
    CloseSidePanel(String),
}

/// The commands of a button menu, depth first, each with whether it can be
/// chosen now: the rows of a menu one level deep. A disabled submenu's
/// commands cannot be.
fn menu_commands<'a>(
    items: &'a [ExtensionMenuItem],
    enabled: bool,
    out: &mut Vec<(&'a ExtensionMenuItem, bool)>,
) {
    for item in items {
        match item {
            ExtensionMenuItem::Separator => {}
            ExtensionMenuItem::Submenu {
                enabled: open,
                items,
                ..
            } => menu_commands(items, enabled && *open, out),
            ExtensionMenuItem::Command { enabled: on, .. } => out.push((item, enabled && *on)),
        }
    }
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

/// One of an extension's own pages the panel shows.
enum ExtensionPage {
    /// Its toolbar button's popup at `url`; `clicked` when the person's
    /// click opened it, which lets the extension script the page.
    Popup {
        id: String,
        url: String,
        clicked: bool,
    },
    Settings {
        id: String,
        url: String,
    },
    /// A web page it asked the person to sign in on
    /// (`identity.launchWebAuthFlow`), which the engine watches: the
    /// sign-in ends, and the page closes, once it goes to the extension's
    /// own address.
    SignIn {
        id: String,
        url: String,
        flow: u32,
    },
}

/// The most sign-ins loaded out of sight at once: an extension checking
/// whether the person is still signed in, which takes a moment.
const QUIET_SIGN_INS: usize = 4;

/// The extension a panel's page belongs to, by the key it is shown under:
/// its popup's is its id, its settings' and a sign-in's carry a prefix.
fn panel_owner(key: &str) -> &str {
    key.strip_prefix("settings:")
        .or_else(|| key.strip_prefix("sign-in:"))
        .unwrap_or(key)
}

/// The site a sign-in page is on, as the panel names it above the page:
/// the person types a password there, so they see whose page it is.
fn sign_in_site(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

/// What an extension's page in the panel opens (its dashboard, a help page)
/// opens beside the window, as Chrome opens it in a tab.
fn open_beside(page: &SharedEngine) {
    for (url, background) in page.take_new_tab_requests() {
        if url.starts_with("https://") || url.starts_with("http://") {
            ui::shell_tabs::open_beside(Some(url), background);
        }
    }
}

/// An icon an extension gave its button through `chrome.action.setIcon`.
#[derive(Clone)]
enum ActionIcon {
    /// Its file at this path, read once a read of the profile says where the
    /// extension's files are: a window opened beside running extensions is
    /// told their icons as its page opens, before it knows them.
    Unread(String),
    /// Its file at this path is being read.
    Reading(String),
    /// What could be drawn of it: nothing when it is no PNG of its own.
    Ready(Option<vello::peniko::ImageData>),
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
    /// An extension the daily check brought up to date.
    Updated(extensions::Updated),
    /// An icon file an extension gave its button, read.
    ActionIcon {
        id: String,
        path: String,
        image: Option<vello::peniko::ImageData>,
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
    /// Icons of extensions the read was told it did not know yet.
    icons: Vec<(String, Option<vello::peniko::ImageData>)>,
}

/// The extensions as the toolbar knows them, beside their records, paired
/// so the Library's row for each shows its own buttons: one Chromium could
/// not name (a broken `key`) is left out of both.
fn paired(infos: Vec<ExtensionInfo>) -> (Vec<ExtensionInfo>, Vec<Installed>) {
    infos
        .into_iter()
        .filter_map(|info| {
            let installed = Installed::read(
                &info,
                extensions::is_approved(&info),
                extensions::is_pinned(&extensions::chrome_id(&info)?),
            )?;
            Some((info, installed))
        })
        .unzip()
}

/// Read the profile, with the icons of the extensions not in `known`.
fn read_profile(known: &HashSet<String>) -> Profile {
    let infos = extensions::list_installed();
    // Each worker runs the tab script before the engine loads it.
    for info in &infos {
        extensions::prepare_tabs(&info.path);
    }
    let (infos, installed) = paired(infos);
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
        icons,
    }
}

/// How often a window asks for the day's update check, which one Browser
/// on the profile is given each day: an ad blocker's rules come with its
/// updates, and the one that checked may since have closed.
const UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What the anchored panel shows.
enum Panel {
    Offer(Box<StagedPackage>),
    Message(String),
    Popup {
        id: String,
        engine: SharedEngine,
        /// The size its page asked for, once it said.
        size: Option<(f32, f32)>,
        /// For a sign-in page, the site it is on now.
        site: Option<String>,
        /// Under a sign-in page, the extension's own page that asked for it
        /// (its settings, its popup): it goes on out of sight to hear how
        /// the sign-in ended, and shows again once the sign-in page closes.
        beneath: Option<Box<Panel>>,
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
    /// Icons extensions set for their buttons, by extension and whether for
    /// every tab (`true`) or this window's page; and each icon file read,
    /// by extension and path, since an extension sets the same few again.
    action_icons: HashMap<(String, bool), ActionIcon>,
    icon_files: HashMap<(String, String), Option<vello::peniko::ImageData>>,
    refresh: bool,
    store_item: Option<web_store::Item>,
    busy: bool,
    panel: Option<Panel>,
    side_panel: Option<SidePanel>,
    file_path: ui::LocalState<String>,
    chromium_version: String,
    /// Extensions to turn on (`true`) or off through the manager extension.
    switches: Vec<(String, bool)>,
    /// What this session turned on or off, so each is asked once.
    switched: HashMap<String, bool>,
    /// What the engine was told each extension may do with the Browser's
    /// tabs, and which extension is the Browser's own.
    access_told: HashMap<String, toolbar::Access>,
    identity_told: Option<String>,
    /// The manager pages at work, and when each was opened.
    switching: Vec<(SharedEngine, ui::time::Instant)>,
    /// When the store was last asked for updates.
    updated_at: Option<ui::time::Instant>,
    /// Extensions an update left off until the person allows more access.
    awaiting: Vec<String>,
    /// What each extension's button menu holds, as the engine told it: the
    /// `chrome.contextMenus` items the extension made for its button.
    menus: HashMap<String, Vec<ExtensionMenuItem>>,
    /// Extensions' requests for more access, for the window to ask the
    /// person ([`Self::take_questions`]).
    questions: Vec<ExtensionQuestion>,
    /// Pages extensions sign in on out of sight, until the engine closes
    /// each as its sign-in ends.
    quiet_sign_ins: Vec<SharedEngine>,
    /// The extension typed to after its keyword in the address, if any.
    omnibox: ExtensionOmnibox,
}

/// The switches `installed` asks for that this session has not made yet,
/// noting them as made: an extension is turned on or off once a session,
/// and again each time the person changes it.
fn switches_for(
    installed: &[Installed],
    switched: &mut HashMap<String, bool>,
) -> Vec<(String, bool)> {
    installed
        .iter()
        .filter(|extension| {
            switched.insert(extension.id.clone(), extension.enabled) != Some(extension.enabled)
        })
        .map(|extension| (extension.id.clone(), extension.enabled))
        .collect()
}

/// The manager page that makes `switches`.
fn manager_url(manager: &str, switches: &[(String, bool)]) -> String {
    let list = |on: bool| {
        switches
            .iter()
            .filter(|(_, enabled)| *enabled == on)
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "chrome-extension://{manager}/manage.html?on={}&off={}",
        list(true),
        list(false)
    )
}

/// What the person allowed an extension after it was added, in their
/// words: the access Chrome would have warned of, or that it has more when
/// none of it is; none when it has nothing more.
fn granted_line(granted: &ExtensionPermissionSet) -> Option<String> {
    if granted.is_empty() {
        return None;
    }
    let lines = extensions::asked_summary(granted);
    Some(if lines.is_empty() {
        CATALOG.t("browser-extension-granted-quiet")
    } else {
        CATALOG.t_args(
            "browser-extension-granted",
            &[("access", ui::i18n::ArgValue::Text(lines.join(", ")))],
        )
    })
}

/// The longest words an extension's keyword takes, as the engine carries
/// them.
const MAX_KEYWORD_TEXT_BYTES: usize = 2048;

/// How long a manager page may take before it is let go.
const SWITCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
            action_icons: HashMap::new(),
            icon_files: HashMap::new(),
            refresh: active,
            store_item: None,
            busy: false,
            panel: None,
            side_panel: None,
            file_path: ui::LocalState::new(String::new()),
            chromium_version: web_store::FALLBACK_CHROMIUM_VERSION.to_owned(),
            switches: Vec::new(),
            switched: HashMap::new(),
            access_told: HashMap::new(),
            identity_told: None,
            switching: Vec::new(),
            updated_at: None,
            awaiting: Vec::new(),
            menus: HashMap::new(),
            questions: Vec::new(),
            quiet_sign_ins: Vec::new(),
            omnibox: ExtensionOmnibox::default(),
        }
    }

    /// The requests for more access extensions made since asked, which the
    /// window asks the person in its shared prompt.
    pub fn take_questions(&mut self) -> Vec<ExtensionQuestion> {
        std::mem::take(&mut self.questions)
    }

    /// An extension asks for more access (`chrome.permissions.request`):
    /// the person is asked what it could then do, as when it was added.
    /// Access Chrome asks nobody about (`storage`) is granted at once, as
    /// there; one asked of an extension that is off, refused.
    fn permission_request(
        &mut self,
        request: u32,
        id: &str,
        asked: &ExtensionPermissionSet,
        engine: &SharedEngine,
    ) {
        let Some(extension) = self
            .installed
            .iter()
            .find(|extension| extension.id == id && extension.enabled)
        else {
            engine.send(WebViewCommand::ExtensionPermissionDecision {
                request,
                allow: false,
            });
            return;
        };
        let lines = extensions::asked_summary(asked);
        if lines.is_empty() {
            engine.send(WebViewCommand::ExtensionPermissionDecision {
                request,
                allow: true,
            });
            return;
        }
        self.questions.push(ExtensionQuestion {
            request,
            name: extension.name.clone(),
            lines,
        });
    }

    /// What an extension holds of what it may ask for changed: kept with
    /// the extension, so it holds it in the next run too. The engine holds
    /// it already and is not told it again.
    fn keep_granted(&mut self, id: &str, granted: ExtensionPermissionSet) {
        if let Some(told) = self.access_told.get_mut(id) {
            told.granted.clone_from(&granted);
        }
        if let Some(extension) = self
            .installed
            .iter_mut()
            .find(|extension| extension.id == id)
        {
            extension.access.granted.clone_from(&granted);
        }
        let Some(info) = self.info_of(id).cloned() else {
            return;
        };
        self.change_profile(move || {
            extensions::set_granted(&info, &granted).map_err(|error| error.to_string())
        });
    }

    /// The person takes back what they allowed the extension after it was
    /// added: the engine holds it to what its manifest requires at once, and
    /// the extension hears of what it lost, as when it gives access up
    /// itself; the Browser keeps that it holds none of it.
    fn revoke(&mut self, id: &str, engine: &SharedEngine) {
        let Some(optional) = self
            .installed
            .iter()
            .find(|extension| extension.id == id && !extension.access.granted.is_empty())
            .map(|extension| extension.access.optional.clone())
        else {
            return;
        };
        self.keep_granted(id, ExtensionPermissionSet::default());
        engine.send(WebViewCommand::ExtensionGrants {
            id: id.to_owned(),
            optional,
            granted: ExtensionPermissionSet::default(),
        });
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

    /// The engine runs `id` no more, or will run a new version of it: what it
    /// set for its button is gone.
    fn forget(&mut self, id: &str) {
        self.host.forget(id);
        self.action_icons.retain(|(icon, _), _| icon != id);
        self.icon_files.retain(|(icon, _), _| icon != id);
    }

    /// What the engine says of extensions' toolbar buttons on this window's
    /// page: one changed, the page's own values went with its document, one
    /// asks to open its popup, or the person pressed its keyboard shortcut;
    /// or of an extension's notification, which the system shows.
    /// Returns whether the interface changed.
    pub fn on_engine_event(&mut self, event: WebViewEvent, engine: &SharedEngine) -> bool {
        if !self.active {
            return false;
        }
        match event {
            WebViewEvent::ExtensionAction { id, global, change } => {
                let mut changed = self.host.action_changed(&id, global, &change);
                if let Some(icon) = change.icon {
                    self.set_icon(id, global, icon);
                    changed = true;
                }
                changed
            }
            WebViewEvent::ExtensionMenu { id, items } => {
                if items.is_empty() {
                    self.menus.remove(&id).is_some()
                } else if self.menus.get(&id) == Some(&items) {
                    false
                } else {
                    self.menus.insert(id, items);
                    true
                }
            }
            WebViewEvent::ExtensionActionReset => {
                let before = self.action_icons.len();
                self.action_icons.retain(|(_, global), _| *global);
                self.host.page_changed() | (self.action_icons.len() != before)
            }
            WebViewEvent::ExtensionOpenPopup { id, url } => {
                let running = self
                    .installed
                    .iter()
                    .any(|extension| extension.id == id && extension.enabled);
                if !running
                    || matches!(&self.panel, Some(Panel::Popup { id: open, .. }) if *open == id)
                {
                    return false;
                }
                self.open_page(
                    ExtensionPage::Popup {
                        id,
                        url,
                        clicked: false,
                    },
                    engine,
                );
                true
            }
            // Its button's keyboard shortcut: as a click on the button.
            WebViewEvent::ExtensionExecuteAction { id } => {
                self.open_action(&id, engine);
                true
            }
            // Shown by the system, named for the extension; one taken away
            // goes also for an extension no longer installed.
            WebViewEvent::ExtensionNotification {
                id,
                notification,
                notice,
            } => {
                let name = self
                    .installed
                    .iter()
                    .find(|extension| extension.id == id && extension.enabled)
                    .map(|extension| extension.name.clone());
                if notice.is_some() && name.is_none() {
                    return false;
                }
                crate::extension_notices::announce(
                    &*ui::host::current().notifications,
                    engine,
                    name.as_deref().unwrap_or_default(),
                    &id,
                    (&notification, notice.as_ref()),
                );
                false
            }
            WebViewEvent::ExtensionPermissionRequest { request, id, asked } => {
                self.permission_request(request, &id, &asked, engine);
                !self.questions.is_empty()
            }
            WebViewEvent::ExtensionPermissions { id, granted } => {
                self.keep_granted(&id, granted);
                false
            }
            // It asks the person to sign in: the page shows where its popup
            // would, in place of whatever the panel showed. One that may
            // not ask them loads out of sight, as in Chrome.
            WebViewEvent::ExtensionSignIn {
                id,
                url,
                interactive,
                flow,
            } => {
                let running = self
                    .installed
                    .iter()
                    .any(|extension| extension.id == id && extension.enabled);
                if !running {
                    return false;
                }
                if interactive {
                    self.open_page(ExtensionPage::SignIn { id, url, flow }, engine);
                    return true;
                }
                if let Ok(page) = engine.create_tab() {
                    page.send(WebViewCommand::ExtensionSignInPanel { id, flow });
                    page.send(WebViewCommand::Navigate(url));
                    // The oldest goes for a new one: its sign-in ends.
                    if self.quiet_sign_ins.len() >= QUIET_SIGN_INS {
                        self.quiet_sign_ins.remove(0);
                    }
                    self.quiet_sign_ins.push(page);
                }
                false
            }
            event @ (WebViewEvent::ExtensionOmniboxSuggestions { .. }
            | WebViewEvent::ExtensionOmniboxDefault { .. }) => self.omnibox.on_engine_event(&event),
            WebViewEvent::ExtensionSidePanel { id, change } => {
                self.side_panel_changed(id, change, engine)
            }
            _ => false,
        }
    }

    /// Show `icon` on the extension's button, for every tab or this page;
    /// `None` takes it back. A file is read off the window's thread.
    fn set_icon(&mut self, id: String, global: bool, icon: Option<ExtensionActionIcon>) {
        let key = (id.clone(), global);
        let Some(icon) = icon else {
            self.action_icons.remove(&key);
            return;
        };
        let path = match icon {
            ExtensionActionIcon::Pixels {
                width,
                height,
                rgba,
            } => {
                let image = vello::peniko::ImageData {
                    data: vello::peniko::Blob::new(Arc::new(rgba)),
                    format: vello::peniko::ImageFormat::Rgba8,
                    alpha_type: vello::peniko::ImageAlphaType::Alpha,
                    width,
                    height,
                };
                self.action_icons
                    .insert(key, ActionIcon::Ready(Some(image)));
                return;
            }
            ExtensionActionIcon::Path(path) => path,
        };
        if let Some(image) = self.icon_files.get(&(id.clone(), path.clone())) {
            self.action_icons
                .insert(key, ActionIcon::Ready(image.clone()));
            return;
        }
        let icon = if self.read_action_icon(id, path.clone()) {
            ActionIcon::Reading(path)
        } else {
            ActionIcon::Unread(path)
        };
        self.action_icons.insert(key, icon);
    }

    /// Read the icon file at `path` among the extension's files off the
    /// window's thread; `false` while the profile has not said where they are.
    fn read_action_icon(&self, id: String, path: String) -> bool {
        let Some(root) = self
            .installed
            .iter()
            .find(|extension| extension.id == id)
            .map(|extension| extension.root.clone())
        else {
            return false;
        };
        let outcomes = self.outcomes.clone();
        std::thread::spawn(move || {
            let image =
                toolbar::icon_within(&root, &path).and_then(|file| toolbar::read_icon(&file));
            if let Ok(mut queue) = outcomes.lock() {
                queue.push(Outcome::ActionIcon { id, path, image });
            }
        });
        true
    }

    /// The icon the extension set for its button on this page, else for
    /// every tab; `None` when it set none, and the manifest's stands.
    fn action_icon(&self, id: &str) -> Option<&vello::peniko::ImageData> {
        [false, true].iter().find_map(|global| {
            match self.action_icons.get(&(id.to_owned(), *global))? {
                ActionIcon::Ready(image) => image.as_ref(),
                ActionIcon::Unread(_) | ActionIcon::Reading(_) => None,
            }
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
            changed = true;
            match outcome {
                Outcome::Profile { refused, profile } => {
                    if let Some(error) = refused {
                        self.panel = Some(Panel::Message(error));
                    }
                    self.apply_profile(profile);
                    continue;
                }
                Outcome::ActionIcon { id, path, image } => {
                    for (key, icon) in &mut self.action_icons {
                        if key.0 == id
                            && matches!(icon, ActionIcon::Reading(reading) if *reading == path)
                        {
                            *icon = ActionIcon::Ready(image.clone());
                        }
                    }
                    if self.icon_files.len() >= 64 {
                        self.icon_files.clear();
                    }
                    self.icon_files.insert((id, path), image);
                    continue;
                }
                Outcome::Staged(Ok(package)) => {
                    self.panel = Some(Panel::Offer(Box::new(package)));
                    self.refresh = true;
                }
                Outcome::Installed(Ok(info)) => {
                    self.panel = Some(Panel::Message(named("browser-extension-added", &info.name)));
                    self.set_approved(info, true);
                }
                Outcome::Updated(updated) => {
                    self.on_updated(updated);
                    // An update is no add: one still downloading stays busy.
                    continue;
                }
                Outcome::Staged(Err(message)) | Outcome::Installed(Err(message)) => {
                    self.panel = Some(Panel::Message(message));
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
        self.check_for_updates(now);
        let shown = badges_shown(&self.installed, ui::window_toolbar::is_hosted());
        self.host.probe(&shown, now);
        self.host.flush(engine);
        self.tell_access(engine);
        changed |= self.switch(engine, now);
        let mut permissions = Vec::new();
        // Their events are not the window's; what they open goes nowhere.
        self.quiet_sign_ins.retain(|page| {
            let closed = page
                .drain_events()
                .into_iter()
                .any(|event| matches!(event, WebViewEvent::TabClosed));
            let _ = page.take_new_tab_requests();
            !closed
        });
        if let Some(Panel::Popup {
            engine: popup,
            size,
            site: shown_site,
            beneath,
            ..
        }) = &mut self.panel
        {
            // The popup is a tab of its own: its page events are not the
            // window's, but they must not pile up. What it asks for is the
            // window's to ask.
            let mut closed = false;
            for event in popup.drain_events() {
                match event {
                    event @ (ui::webview::WebViewEvent::ExtensionPermissionRequest { .. }
                    | ui::webview::WebViewEvent::ExtensionPermissions { .. }) => {
                        permissions.push(event);
                    }
                    // Its page closed it (a password filled, a choice made),
                    // or a sign-in reached its end.
                    ui::webview::WebViewEvent::TabClosed => closed = true,
                    // A sign-in page names the site it moved on to.
                    ui::webview::WebViewEvent::UriChanged(url) if shown_site.is_some() => {
                        let now = sign_in_site(&url);
                        if now.is_some() && now != *shown_site {
                            *shown_site = now;
                            changed = true;
                        }
                    }
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
            open_beside(popup);
            // The page under a sign-in asks the window for what it needs,
            // and is gone once it closes itself.
            if let Some(Panel::Popup { engine: under, .. }) = beneath.as_deref() {
                let mut gone = false;
                for event in under.drain_events() {
                    match event {
                        event @ (WebViewEvent::ExtensionPermissionRequest { .. }
                        | WebViewEvent::ExtensionPermissions { .. }) => permissions.push(event),
                        WebViewEvent::TabClosed => gone = true,
                        _ => {}
                    }
                }
                open_beside(under);
                if gone {
                    *beneath = None;
                }
            }
            if closed {
                let under = beneath.take();
                self.panel = under.map(|page| *page);
                changed = true;
            }
        }
        for event in permissions {
            changed |= self.on_engine_event(event, engine);
        }
        changed |= self.sync_side_panel(engine);
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
                tracing::warn!(%error, "the extensions' worker did not start");
                self.panel = Some(Panel::Message(CATALOG.t("browser-extension-unreadable")));
            }
        }
    }

    /// Bring the engine and the window in line with a read of the profile.
    fn apply_profile(&mut self, profile: Profile) {
        self.host.reconcile(&profile.wanted, &[], ui::time::now());
        // On and off through the manager, which keeps an extension's data:
        // each once a session, so one Chromium remembers from an earlier
        // run is in the state the person chose.
        for (id, enabled) in switches_for(&profile.installed, &mut self.switched) {
            if !enabled {
                self.forget(&id);
            }
            self.switches.push((id, enabled));
        }
        // A page of an extension turned off or removed closes with it, and
        // an update waits for the person only while its extension is off.
        if let Some(Panel::Popup { id, .. }) = &self.panel {
            let owner = panel_owner(id);
            if !profile
                .installed
                .iter()
                .any(|extension| extension.id == owner && extension.enabled)
            {
                self.panel = None;
            }
        }
        self.keep_side_panel(&profile.installed);
        self.awaiting.retain(|name| {
            profile
                .installed
                .iter()
                .any(|extension| &extension.name == name && !extension.enabled)
        });
        self.icons
            .retain(|id, _| profile.installed.iter().any(|e| &e.id == id));
        // An extension turned off starts again with its manifest's button.
        let running: HashSet<&String> = profile
            .installed
            .iter()
            .filter(|extension| extension.enabled)
            .map(|extension| &extension.id)
            .collect();
        self.action_icons.retain(|(id, _), _| running.contains(id));
        self.icon_files.retain(|(id, _), _| running.contains(id));
        self.icons.extend(profile.icons);
        self.installed = profile.installed;
        self.infos = profile.infos;
        // Icons told before the profile said where their files are.
        let unread: HashSet<(String, String)> = self
            .action_icons
            .iter()
            .filter_map(|((id, _), icon)| match icon {
                ActionIcon::Unread(path) => Some((id.clone(), path.clone())),
                _ => None,
            })
            .collect();
        for (id, path) in unread {
            if self.read_action_icon(id.clone(), path.clone()) {
                for ((owner, _), icon) in &mut self.action_icons {
                    if *owner == id && matches!(icon, ActionIcon::Unread(unread) if *unread == path)
                    {
                        *icon = ActionIcon::Reading(path.clone());
                    }
                }
            }
        }
    }

    /// An extension the daily check brought up to date: the engine runs the
    /// new version once it loads it again, which the next read of the
    /// profile asks of it, and the Library shows the new version.
    fn on_updated(&mut self, updated: extensions::Updated) {
        let (info, awaits) = match updated {
            extensions::Updated::Installed(info) => (info, false),
            extensions::Updated::AwaitsApproval(info) => (info, true),
        };
        if let Some(id) = extensions::chrome_id(&info) {
            self.forget(&id);
        }
        if awaits {
            self.awaiting.push(info.name);
        }
        self.refresh = true;
    }

    /// Ask the Web Store for newer versions of its extensions, on a worker,
    /// when this Browser is given the day's check.
    fn check_for_updates(&mut self, now: ui::time::Instant) {
        if self.busy
            || self.infos.is_empty()
            || self
                .updated_at
                .is_some_and(|at| now.saturating_duration_since(at) < UPDATE_INTERVAL)
        {
            return;
        }
        self.updated_at = Some(now);
        let infos = self.infos.clone();
        let version = self.chromium_version.clone();
        let outcomes = self.outcomes.clone();
        std::thread::spawn(move || {
            let root = extensions::extensions_dir();
            if !extensions::claim_update_check(&root, ui::time::unix_now()) {
                return;
            }
            for info in infos {
                match extensions::update_from_store(&info, &version, &root) {
                    Ok(Some(updated)) => {
                        if let Ok(mut queue) = outcomes.lock() {
                            queue.push(Outcome::Updated(updated));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, extension = %info.name, "extension update failed");
                    }
                }
            }
        });
    }

    /// Tell the engine which extension is the Browser's own, and what each
    /// installed extension may do with the Browser's tabs, as it changes.
    fn tell_access(&mut self, engine: &SharedEngine) {
        if self.identity_told.is_none()
            && let Some(dir) = extensions::manager_dir()
            && let Some(id) = extensions::manager_id(&dir)
        {
            engine.send(WebViewCommand::ExtensionIdentity { id: id.clone() });
            // Its worker tells the engine what extensions' rules acted on in
            // the tabs, which it counts for each page.
            if let Some(token) = extensions::manager_token(&dir) {
                engine.send(WebViewCommand::ExtensionAccess {
                    id: id.clone(),
                    permissions: Vec::new(),
                    hosts: Vec::new(),
                    scriptable: Vec::new(),
                    token: Some(token),
                    commands: None,
                });
            }
            self.identity_told = Some(id);
        }
        // A turned-off extension does not run, so its keyboard shortcuts are
        // no one's: the page keeps those keys, and another extension may
        // have them. Turned on again, it gets them back in its place.
        let effective = |extension: &Installed| {
            let mut access = extension.access.clone();
            if !extension.enabled {
                access.commands = None;
            }
            access
        };
        let current: HashMap<String, toolbar::Access> = self
            .installed
            .iter()
            .map(|extension| (extension.id.clone(), effective(extension)))
            .collect();
        // In the order they were installed: of two extensions suggesting
        // one keyboard shortcut, the engine gives it to the one told first.
        for extension in &self.installed {
            let (id, access) = (&extension.id, &effective(extension));
            let told = self.access_told.get(id);
            if told != Some(access) {
                engine.send(WebViewCommand::ExtensionAccess {
                    id: id.clone(),
                    permissions: access.permissions.clone(),
                    hosts: access.hosts.clone(),
                    scriptable: access.scriptable.clone(),
                    token: access.token.clone(),
                    commands: access.commands.clone(),
                });
                // What it may ask for later, and was granted: told when it
                // has any, or had before.
                let asks = |access: &toolbar::Access| {
                    !access.optional.is_empty() || !access.granted.is_empty()
                };
                if asks(access) || told.is_some_and(asks) {
                    engine.send(WebViewCommand::ExtensionGrants {
                        id: id.clone(),
                        optional: access.optional.clone(),
                        granted: access.granted.clone(),
                    });
                }
            }
        }
        for id in self.access_told.keys() {
            if !current.contains_key(id) {
                engine.send(WebViewCommand::ExtensionForget { id: id.clone() });
            }
        }
        self.access_told = current;
    }

    /// Open one manager page for the switches asked for, and let go of the
    /// pages that are done. Returns whether anything changed.
    fn switch(&mut self, engine: &SharedEngine, now: ui::time::Instant) -> bool {
        let before = self.switching.len();
        self.switching.retain(|(page, opened)| {
            let done = page.drain_events().into_iter().any(|event| {
                matches!(event, ui::webview::WebViewEvent::TitleChanged(title) if title.starts_with("done"))
            });
            !done && now.saturating_duration_since(*opened) < SWITCH_TIMEOUT
        });
        let mut changed = self.switching.len() != before;
        if self.switches.is_empty() {
            return changed;
        }
        let Some(manager) = extensions::manager_dir()
            .as_deref()
            .and_then(extensions::manager_id)
        else {
            return changed;
        };
        let url = manager_url(&manager, &self.switches);
        if let Ok(page) = engine.create_tab() {
            // Not a tab any extension is shown.
            page.send(WebViewCommand::ExtensionPanel { action: None });
            page.send(WebViewCommand::Navigate(url));
            self.switching.push((page, now));
            self.switches.clear();
            changed = true;
        }
        changed
    }

    /// The record of the extension Chromium knows as `id`, from the last
    /// read of the profile: never the disk, since this runs on the frame.
    fn info_of(&self, id: &str) -> Option<&ExtensionInfo> {
        self.infos
            .iter()
            .zip(&self.installed)
            .find(|(_, extension)| extension.id == id)
            .map(|(info, _)| info)
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
                        .map_err(|error| add_failed(&error));
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
                        extensions::stage_file(bytes).map_err(|error| add_failed(&error))
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
                            .map_err(ExtensionError::from)
                            .and_then(|()| package.install(&root))
                            .map_err(|error| add_failed(&error));
                        if let Ok(mut queue) = outcomes.lock() {
                            queue.push(Outcome::Installed(installed));
                        }
                    });
                }
            }
            Command::Cancel | Command::ClosePanel => self.panel = None,
            Command::Enable(id) => {
                if let Some(name) = self.info_of(&id).map(|info| info.name.clone()) {
                    self.awaiting.retain(|waiting| *waiting != name);
                }
                self.set_enabled(&id, true);
            }
            Command::Disable(id) => self.set_enabled(&id, false),
            Command::Remove(id) => {
                if let Some(info) = self.info_of(&id).cloned() {
                    self.host
                        .reconcile(&[], std::slice::from_ref(&id), ui::time::now());
                    self.forget(&id);
                    self.change_profile(move || {
                        extensions::uninstall(&info, &extensions::extensions_dir()).map_err(
                            |error| {
                                let name = ui::i18n::ArgValue::Text(info.name.clone());
                                refusal(
                                    &error,
                                    "browser-extension-remove-failed",
                                    &[("name", name)],
                                )
                            },
                        )
                    });
                }
            }
            Command::Revoke(id) => self.revoke(&id, engine),
            Command::Pin(id, pinned) => {
                let name = self
                    .installed
                    .iter()
                    .find(|extension| extension.id == id)
                    .map_or_else(|| id.clone(), |extension| extension.name.clone());
                self.change_profile(move || {
                    extensions::set_pinned(&id, pinned).map_err(|error| {
                        let args = [
                            ("name", ui::i18n::ArgValue::Text(name)),
                            ("pinned", ui::i18n::ArgValue::Text(pinned.to_string())),
                        ];
                        refusal(&error, "browser-extension-pin-failed", &args)
                    })
                });
            }
            Command::OpenAction(id) => self.open_action(&id, engine),
            Command::OpenSettings(id) => self.open_settings(&id, engine),
            Command::ChooseMenuItem(id, key) => {
                if self.menus.contains_key(&id) {
                    engine.send(WebViewCommand::ExtensionMenuChoice { id, key });
                }
            }
            Command::Send(id, text) => engine.send(WebViewCommand::ExtensionOmnibox {
                id,
                input: ui::webview::OmniboxInput::Entered { text },
            }),
            Command::OpenSidePanel(id) => self.open_side_panel(id, engine),
            Command::CloseSidePanel(id) => self.close_side_panel(&id),
            // Busy: ignore a second fetch until the first settles.
            Command::Fetch(_) | Command::StageFile(_) => {}
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(info) = self.info_of(id).cloned() {
            self.set_approved(info, enabled);
        }
    }

    /// Turn the extension on or off on the profile's worker.
    fn set_approved(&mut self, info: ExtensionInfo, enabled: bool) {
        self.change_profile(move || {
            extensions::set_approved(&info, enabled).map_err(|error| {
                let key = if enabled {
                    "browser-extension-turn-on-failed"
                } else {
                    "browser-extension-turn-off-failed"
                };
                refusal(
                    &error,
                    key,
                    &[("name", ui::i18n::ArgValue::Text(info.name))],
                )
            })
        });
    }

    fn open_action(&mut self, id: &str, engine: &SharedEngine) {
        // As the button shows on this page: its popup, or none, or off.
        let Some(button) = self
            .model()
            .menu
            .into_iter()
            .map(|entry| entry.button)
            .find(|button| button.id == id)
        else {
            return;
        };
        if !button.enabled {
            return;
        }
        let Some(url) = button.popup else {
            // A button without a popup acts on the page itself: the
            // extension hears of the click, as in Chrome.
            self.panel = None;
            engine.send(WebViewCommand::ExtensionClick { id: id.to_owned() });
            return;
        };
        if matches!(&self.panel, Some(Panel::Popup { id: open, .. }) if open == id) {
            self.panel = None;
            return;
        }
        self.open_page(
            ExtensionPage::Popup {
                id: id.to_owned(),
                url,
                clicked: true,
            },
            engine,
        );
    }

    /// Show an extension's settings page, as large as the window allows up
    /// to Chrome's largest panel: an ad blocker's lists need the room.
    fn open_settings(&mut self, id: &str, engine: &SharedEngine) {
        let Some(url) = self
            .installed
            .iter()
            .find(|extension| extension.id == id)
            .and_then(Installed::options_url)
        else {
            return;
        };
        self.open_page(
            ExtensionPage::Settings {
                id: id.to_owned(),
                url,
            },
            engine,
        );
    }

    /// Open one of an extension's own pages, or a page it asks the person to
    /// sign in on, in the panel, in a tab of its own.
    fn open_page(&mut self, kind: ExtensionPage, engine: &SharedEngine) {
        // Its "current tab" is the page it opened over, which the extension
        // may script when its button opened it. A sign-in page keeps the
        // size it is given: it is a site's, sized for a window.
        let (key, url, mark, size, heading) = match kind {
            ExtensionPage::Popup { id, url, clicked } => (
                id.clone(),
                url,
                WebViewCommand::ExtensionPanel {
                    action: clicked.then_some(id),
                },
                None,
                None,
            ),
            ExtensionPage::Settings { id, url } => (
                format!("settings:{id}"),
                url,
                WebViewCommand::ExtensionPanel { action: None },
                Some(POPUP_MAX),
                None,
            ),
            ExtensionPage::SignIn { id, url, flow } => {
                let heading = Some(sign_in_site(&url).unwrap_or_default());
                (
                    format!("sign-in:{id}"),
                    url,
                    WebViewCommand::ExtensionSignInPanel { id, flow },
                    Some(POPUP_MAX),
                    heading,
                )
            }
        };
        match engine.create_tab() {
            Ok(page) => {
                let sign_in = heading.is_some();
                let beneath = if sign_in {
                    self.asked_from(panel_owner(&key))
                } else {
                    None
                };
                page.send(mark);
                if !sign_in {
                    page.send(WebViewCommand::ReportDocumentSize(true));
                }
                page.send(WebViewCommand::Navigate(url));
                self.panel = Some(Panel::Popup {
                    id: key,
                    engine: page,
                    size,
                    site: heading,
                    beneath,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "an extension's page did not open");
                self.panel = Some(Panel::Message(CATALOG.t("browser-extension-page-failed")));
            }
        }
    }

    /// The page in the panel that extension `asker`'s sign-in shows over,
    /// to keep beneath it: the extension's own page, which asked for it
    /// and waits to hear how it ends, or the page under its earlier
    /// sign-in. Another extension's page closes, as Chrome's popup closes.
    fn asked_from(&mut self, asker: &str) -> Option<Box<Panel>> {
        let Some(Panel::Popup {
            id,
            engine,
            size,
            site,
            beneath,
        }) = self.panel.take()
        else {
            return None;
        };
        if panel_owner(&id) != asker {
            return None;
        }
        if site.is_some() {
            return beneath;
        }
        Some(Box::new(Panel::Popup {
            id,
            engine,
            size,
            site,
            beneath,
        }))
    }

    /// The width an open extension page asks for, within `room`; `None`
    /// when the panel shows no page. The panel is as wide as its page.
    pub fn page_width(&self, room: f32) -> Option<f32> {
        match &self.panel {
            Some(Panel::Popup { size, .. }) => Some(size.unwrap_or(POPUP_SIZE).0.min(room)),
            _ => None,
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
            let label = CATALOG.t(if self.busy {
                "browser-extension-adding"
            } else {
                "browser-extension-add"
            });
            buttons.push(Box::new(
                Button::new(ADD_BUTTON, &label)
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
            .with_enabled(entry.enabled)
            .on_effect(self.queue(Command::OpenAction(entry.id.clone())));
            let image = self
                .action_icon(&entry.id)
                .or_else(|| self.icons.get(&entry.id).and_then(Option::as_ref));
            if let Some(image) = image {
                button = button.with_image(image.clone(), Some(&entry.badge));
            }
            buttons.push(Box::new(button));
        }
        buttons.extend(self.side_panel_button());
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
    /// which opens its popup, as the Extensions menu's rows do, each followed
    /// by what its button menu offers, as far as the bar's menu has room
    /// beside its last item. The bar's menu is one level deep: a submenu's
    /// items stand in its place.
    pub fn bar_items(&self) -> Vec<ToolbarAction> {
        if !self.active {
            return Vec::new();
        }
        let entries = self.model().menu;
        let side_panels = self.side_panel_entries();
        let mut room = ui::window_toolbar::MAX_ITEMS
            .saturating_sub(1)
            .saturating_sub(entries.len())
            .saturating_sub(side_panels.len());
        let mut items = Vec::new();
        for entry in entries {
            let name = if entry.button.badge.is_empty() {
                entry.button.label
            } else {
                format!("{} ({})", entry.button.label, entry.button.badge)
            };
            let id = entry.button.id;
            items.push(
                ToolbarAction::new(format!("{BAR_OPEN}{id}"), "box", name)
                    .enabled(entry.button.enabled),
            );
            let mut commands = Vec::new();
            menu_commands(
                self.menus.get(&id).map_or(&[][..], Vec::as_slice),
                true,
                &mut commands,
            );
            for (command, enabled) in commands.into_iter().take(room) {
                let ExtensionMenuItem::Command {
                    key,
                    label,
                    checked,
                    ..
                } = command
                else {
                    continue;
                };
                room -= 1;
                let item = ToolbarAction::new(format!("{BAR_MENU}{id}{key}"), "", label.clone())
                    .enabled(enabled);
                items.push(match checked {
                    Some(on) => item.on(*on),
                    None => item,
                });
            }
        }
        items.extend(side_panel::bar_items(side_panels));
        items
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
        } else if let Some((extension, key)) = id
            .strip_prefix(BAR_MENU)
            .and_then(|rest| rest.split_at_checked(EXTENSION_ID_LEN))
        {
            Command::ChooseMenuItem(extension.to_owned(), key.to_owned())
        } else if let Some(command) = self.side_panel_bar_command(id) {
            command
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
            items.push(
                MenuItem::action(&label, move || (open.borrow_mut())())
                    .with_enabled(entry.button.enabled),
            );
            let pin = self.queue(Command::Pin(entry.button.id.clone(), !entry.pinned));
            let pin = std::cell::RefCell::new(pin);
            items.push(
                MenuItem::action(
                    named("browser-extension-pin-to-toolbar", &entry.button.label),
                    move || (pin.borrow_mut())(),
                )
                .with_checked(entry.pinned),
            );
            // What the extension made for its button's menu.
            if let Some(menu) = self.menus.get(&entry.button.id) {
                items.push(MenuItem::submenu(
                    format!("More from {}", entry.button.label),
                    self.menu_rows(&entry.button.id, menu),
                ));
            }
        }
        items.extend(self.side_panel_menu_items());
        let name = CATALOG.t("browser-menu-extensions");
        Button::icon_only(MENU_BUTTON, Icon::Box, &name)
            .tooltip(&name)
            .with_menu(items)
            .aligned(PanelAlign::Trailing)
    }

    /// An extension's button menu as rows: check marks on its checkbox and
    /// radio items, its submenus nested, its disabled items greyed.
    fn menu_rows(&self, extension: &str, items: &[ExtensionMenuItem]) -> Vec<MenuItem> {
        items
            .iter()
            .map(|item| match item {
                ExtensionMenuItem::Separator => MenuItem::separator(),
                ExtensionMenuItem::Submenu {
                    label,
                    enabled,
                    items,
                } => MenuItem::submenu(label, self.menu_rows(extension, items))
                    .with_enabled(*enabled),
                ExtensionMenuItem::Command {
                    key,
                    label,
                    checked,
                    enabled,
                } => {
                    let choose = std::cell::RefCell::new(
                        self.queue(Command::ChooseMenuItem(extension.to_owned(), key.clone())),
                    );
                    let row = MenuItem::action(label, move || (choose.borrow_mut())())
                        .with_enabled(*enabled);
                    match checked {
                        Some(checked) => row.with_checked(*checked),
                        None => row,
                    }
                }
            })
            .collect()
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
                        Text::new(&named("browser-extension-offer", &package.info.name))
                            .with_role(ui::tokens::TextRole::Heading),
                    ))
                    .push(text(&CATALOG.t_args(
                        "browser-extension-version",
                        &[(
                            "version",
                            ui::i18n::ArgValue::Text(package.info.version.clone()),
                        )],
                    )))
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
                        Button::new(
                            "browser.extension.confirm",
                            &CATALOG.t("browser-extension-confirm"),
                        )
                        .on_effect(self.queue(Command::Confirm)),
                    )
                    .secondary(
                        Button::new(
                            "browser.extension.cancel",
                            &CATALOG.t("browser-extension-cancel"),
                        )
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
                            Button::new(
                                "browser.extension.close",
                                &CATALOG.t("browser-extension-close"),
                            )
                            .on_effect(self.queue(Command::ClosePanel)),
                        )),
                ),
                vec![ADD_BUTTON.to_owned(), MENU_BUTTON.to_owned()],
            ),
            Panel::Popup {
                id,
                engine,
                size,
                site: heading,
                ..
            } => {
                let view = ui::widget::WebView::new(engine.clone())
                    .with_test_id("browser.extension.popup");
                let page = size.unwrap_or(POPUP_SIZE);
                let size = (page.0.min(width), page.1.min(max_height));
                // A sign-in page under the name of the site it is on.
                let content: Element = match heading {
                    Some(name) => Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_rhythm(ui::tokens::Rhythm::Sibling)
                            .push(Box::new(
                                Text::new(name)
                                    .with_role(ui::tokens::TextRole::Caption)
                                    .dimmed(),
                            ))
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .with_flex_grow()
                                    .with_child(Box::new(view)),
                            )),
                    ),
                    None => Box::new(view),
                };
                (
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(size.0)
                            .with_height(size.1)
                            .with_child(content),
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
        let status = self.status(info, extension);
        // Its broadest access, the line Chrome lists first.
        let permissions = extensions::permission_summary(info).map_or_else(
            |_| CATALOG.t("browser-extension-access-unknown"),
            |lines| lines.into_iter().next().unwrap_or_default(),
        );
        let mut buttons = Vec::new();
        // What the person allowed it after it was added, which they can take
        // back here.
        let granted = granted_line(&extension.access.granted);
        let mut lines = vec![format!("{status}. {permissions}")];
        lines.extend(granted.clone());
        if info.manifest_version == 3 {
            let command = if extension.enabled {
                Command::Disable(id.clone())
            } else {
                Command::Enable(id.clone())
            };
            buttons.push(
                Button::new(
                    &format!("browser.extension.toggle.{id}"),
                    &CATALOG.t(if extension.enabled {
                        "browser-extension-turn-off"
                    } else {
                        "browser-extension-turn-on"
                    }),
                )
                .on_effect(self.queue(command)),
            );
        }
        if extension.enabled && extension.options.is_some() {
            buttons.push(
                Button::new(
                    &format!("browser.extension.settings.{id}"),
                    &CATALOG.t("browser-extension-settings"),
                )
                .on_effect(self.queue(Command::OpenSettings(id.clone()))),
            );
        }
        if extension.enabled && extension.action.is_some() {
            buttons.push(
                Button::new(
                    &format!("browser.extension.pin.{id}"),
                    &CATALOG.t(if extension.pinned {
                        "browser-extension-unpin"
                    } else {
                        "browser-extension-pin"
                    }),
                )
                .on_effect(self.queue(Command::Pin(id.clone(), !extension.pinned))),
            );
        }
        if granted.is_some() {
            buttons.push(
                Button::new(
                    &format!("browser.extension.revoke.{id}"),
                    &CATALOG.t("browser-extension-revoke"),
                )
                .on_effect(self.queue(Command::Revoke(id.clone()))),
            );
        }
        buttons.push(
            Button::new(
                &format!("browser.extension.remove.{id}"),
                &CATALOG.t("browser-extension-remove"),
            )
            .on_effect(self.queue(Command::Remove(id.clone()))),
        );
        Box::new(crate::rows::with_buttons(
            ListRow::new(format!("browser.extension.{id}"), info.name.clone())
                .secondary(lines.join("\n"))
                .secondary_lines(u8::try_from(lines.len() + 1).unwrap_or(u8::MAX))
                .meta(info.version.clone()),
            buttons,
        ))
    }

    /// Whether an extension runs, as its Library row and its resource say.
    fn status(&self, info: &ExtensionInfo, extension: &Installed) -> String {
        CATALOG.t(if info.manifest_version != 3 {
            "browser-extension-unsupported"
        } else if self.host.is_loaded(&extension.id) {
            "browser-extension-running"
        } else if extension.enabled {
            "browser-extension-starting"
        } else {
            "browser-extension-off"
        })
    }

    /// The installed extensions as resources, each with what its Library
    /// row offers, so a screen reader, a script or an agent manages them as
    /// the person does.
    pub fn resources(&self) -> Vec<Resource> {
        use crate::effects::BrowserEffect as E;
        use ui::tilo_resource::Effect as _;
        self.infos
            .iter()
            .zip(&self.installed)
            .map(|(info, extension)| {
                let id = extension.id.clone();
                let granted = granted_line(&extension.access.granted);
                let mut resource = Resource::new(
                    extensions::resource_id(&info.id),
                    "browser.extension",
                    info.name.clone(),
                )
                .with_presentation(ResourcePresentation {
                    detail: Some(self.status(info, extension)),
                    // What the person allowed it later, as its row says.
                    preview: granted
                        .as_ref()
                        .filter(|line| line.len() <= ResourcePresentation::MAX_PREVIEW_BYTES)
                        .cloned(),
                    ..ResourcePresentation::default()
                })
                .with_action(E::ShowExtensions.bound());
                if granted.is_some() {
                    resource =
                        resource.with_action(E::RevokeExtensionAccess { id: id.clone() }.bound());
                }
                if info.manifest_version == 3 {
                    resource = resource.with_action(if extension.enabled {
                        E::TurnOffExtension { id: id.clone() }.bound()
                    } else {
                        E::TurnOnExtension { id: id.clone() }.bound()
                    });
                }
                if extension.enabled && extension.action.is_some() {
                    resource = resource.with_action(E::UseExtension { id: id.clone() }.bound());
                }
                if extension.enabled && extension.options.is_some() {
                    resource =
                        resource.with_action(E::OpenExtensionSettings { id: id.clone() }.bound());
                }
                if extension.enabled && extension.keyword.is_some() {
                    resource = resource.with_action(
                        E::SendToExtension {
                            id: id.clone(),
                            text: String::new(),
                        }
                        .bound_asking(&["text"]),
                    );
                }
                if let Some(action) = self.side_panel_action(extension) {
                    resource = resource.with_action(action);
                }
                if extension.enabled && extension.action.is_some() {
                    resource = resource.with_action(if extension.pinned {
                        E::UnpinExtension { id: id.clone() }.bound()
                    } else {
                        E::PinExtension { id: id.clone() }.bound()
                    });
                }
                resource.with_action(E::RemoveExtension { id }.bound())
            })
            .collect()
    }

    /// Do what an extension's action asks, on the next `sync`, as its
    /// Library row's button does.
    ///
    /// # Errors
    /// When this window has no extensions, no extension has the id, or the
    /// extension cannot do it now (a settings page while it is off).
    pub fn request(&mut self, request: &ExtensionRequest) -> Result<(), String> {
        if !self.active {
            return Err(CATALOG.t("browser-extension-window-none"));
        }
        let id = match request {
            ExtensionRequest::TurnOn(id)
            | ExtensionRequest::TurnOff(id)
            | ExtensionRequest::Pin(id, _)
            | ExtensionRequest::OpenSettings(id)
            | ExtensionRequest::Use(id)
            | ExtensionRequest::Send(id, _)
            | ExtensionRequest::OpenSidePanel(id)
            | ExtensionRequest::CloseSidePanel(id)
            | ExtensionRequest::Revoke(id)
            | ExtensionRequest::Remove(id) => id.clone(),
        };
        let (info, extension) = self
            .infos
            .iter()
            .zip(&self.installed)
            .find(|(_, extension)| extension.id == id)
            .ok_or_else(|| CATALOG.t("browser-extension-not-added"))?;
        let command = match request {
            ExtensionRequest::TurnOn(_) if info.manifest_version != 3 => {
                return Err(CATALOG.t("browser-extension-runs-no-more"));
            }
            ExtensionRequest::TurnOn(_) => Command::Enable(id),
            ExtensionRequest::TurnOff(_) => Command::Disable(id),
            ExtensionRequest::Pin(..) | ExtensionRequest::Use(_)
                if !extension.enabled || extension.action.is_none() =>
            {
                return Err(CATALOG.t("browser-extension-no-button"));
            }
            ExtensionRequest::Pin(_, pinned) => Command::Pin(id, *pinned),
            ExtensionRequest::OpenSettings(_)
                if !extension.enabled || extension.options.is_none() =>
            {
                return Err(CATALOG.t("browser-extension-no-settings"));
            }
            ExtensionRequest::OpenSettings(_) => Command::OpenSettings(id),
            ExtensionRequest::Use(_) => Command::OpenAction(id),
            ExtensionRequest::Send(..) if !extension.enabled || extension.keyword.is_none() => {
                return Err("This extension is off or has no keyword".into());
            }
            ExtensionRequest::Send(_, text) if text.len() > MAX_KEYWORD_TEXT_BYTES => {
                return Err(format!(
                    "The words must be at most {MAX_KEYWORD_TEXT_BYTES} bytes"
                ));
            }
            ExtensionRequest::Send(_, text) => Command::Send(id, text.clone()),
            ExtensionRequest::OpenSidePanel(_) | ExtensionRequest::CloseSidePanel(_) => {
                side_panel::command(request, extension)?
            }
            ExtensionRequest::Revoke(_) if extension.access.granted.is_empty() => {
                return Err(CATALOG.t("browser-extension-nothing-granted"));
            }
            ExtensionRequest::Revoke(_) => Command::Revoke(id),
            ExtensionRequest::Remove(_) => Command::Remove(id),
        };
        (self.queue(command))();
        Ok(())
    }

    /// Adding the Web Store item on the page, as the extensions' resource
    /// offers it: available on an item not added yet; none in a window that
    /// manages no extensions.
    pub fn add_action(&self) -> Option<ui::tilo_actions::ActionSpec> {
        use ui::tilo_resource::Effect as _;
        let add = crate::effects::BrowserEffect::AddExtension.bound();
        self.active.then(|| match self.store_item_to_add() {
            Ok(_) => add,
            Err((code, reason)) => add.unavailable(code, reason),
        })
    }

    /// Show what the Web Store item on the page may do, on the next `sync`,
    /// as Add to Tilo Browser does: the person adds it from the offer.
    ///
    /// # Errors
    /// When there is no Web Store item on the page, it is added already or
    /// another is being added.
    pub fn offer_store_item(&self) -> Result<(), String> {
        if !self.active {
            return Err(CATALOG.t("browser-extension-window-none"));
        }
        let id = self
            .store_item_to_add()
            .map_err(|(_, reason)| reason)?
            .id
            .clone();
        (self.queue(Command::Fetch(id)))();
        Ok(())
    }

    /// The Web Store item on the page, when it can be added now; otherwise
    /// why not, as a stable code and the reason a person or agent reads.
    /// The action's availability and its invocation share it, so an agent
    /// is never told to open a store page while it is on one.
    fn store_item_to_add(&self) -> Result<&web_store::Item, (&'static str, String)> {
        let item = self.store_item.as_ref().ok_or_else(|| {
            (
                "no_store_item",
                CATALOG.t("browser-extension-open-store-first"),
            )
        })?;
        if self
            .installed
            .iter()
            .any(|extension| extension.id == item.id)
        {
            return Err(("added", CATALOG.t("browser-extension-already-added")));
        }
        if self.busy {
            return Err(("adding", CATALOG.t("browser-extension-another-adding")));
        }
        Ok(item)
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
                    Text::new(&CATALOG.t("browser-extension-how-to-add"))
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
                                        .with_accessible_name(CATALOG.t("browser-extension-file"))
                                        .with_placeholder(
                                            CATALOG.t("browser-extension-file-placeholder"),
                                        ),
                                )),
                        ))
                        .push(Box::new(
                            Button::new(
                                "browser.extension.install-file",
                                &CATALOG.t("browser-extension-install-file"),
                            )
                            .on_effect(install),
                        )),
                )),
        )
    }

    pub fn notice(&self) -> Option<Element> {
        match &self.panel {
            Some(Panel::Message(message)) => Some(Box::new(Banner::info(message))),
            _ => self.awaiting.first().map(|name| -> Element {
                Box::new(Banner::info(named("browser-extension-update-asks", name)))
            }),
        }
    }
}

/// The package file at `path`, or what the person reads when it cannot be
/// read: no such file, one they may not read, or one too large.
fn read_package_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    const LIMIT: u64 = 256 * 1024 * 1024;
    let unreadable = |error: std::io::Error| {
        tracing::warn!(%error, path = %path.display(), "an extension file could not be read");
        CATALOG.t(if error.kind() == std::io::ErrorKind::NotFound {
            "browser-extension-file-missing"
        } else {
            "browser-extension-file-unreadable"
        })
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > LIMIT {
        return Err(ExtensionError::TooLarge.message());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
