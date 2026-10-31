//! `SettingsApp` — what the Settings window shares with its view: the
//! areas the panes read, the settings document and the sections' actions,
//! the pane shown and what asks for another.
//!
//! The window's view ([`crate::window::window`]) draws it, answers the
//! effects and publishes the live rows from it. Everything here is read through a shared reference, so the
//! effect handler, the published rows and the window hold it together.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::categories::Category;

/// A pane's scroll offset, kept per category while the window is open.
pub(crate) type ScrollOffset = Rc<Cell<(f32, f32)>>;

/// What the window, its effects and its rows share. See the module docs.
pub struct SettingsApp {
    /// This app, as its jobs reach it again when they finish.
    me: Weak<SettingsApp>,
    /// The pane the window shows.
    selected: Cell<Category>,
    /// The app's storage: its settings document, files and secrets.
    storage: ui::storage::AppStorage,
    /// The stand-ins a screenshot shows; `None` in a live window.
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    /// Whether the areas reach this device's hardware and services
    /// ([`crate::area::AreaContext::devices`]).
    devices: bool,
    /// The radios this device has; never read when it reaches no device.
    radios: crate::radios::Radios,
    /// The pages the panes drilled into.
    pages: crate::pages::PanePages,
    /// The password prompt the window shows for the panes.
    secrets: crate::secrets::SecretAsks,
    provider_admin: ui::models::ProviderAdministration,
    /// The person's linked accounts, when the host lent them.
    linked: ui::linked_accounts::LinkedAccounts,
    /// The settings sections' actions, once attached to a view.
    sections: crate::sections::Sections,
    /// The pane a row, the palette or an agent asked for, taken by the
    /// window on its next sync.
    sidebar_selection: ui::LocalState<Option<Category>>,
    /// The setting asked to be shown, with the pane asked for.
    setting_request: ui::LocalState<Option<&'static settings_store::index::Setting>>,
    /// The scenario a capture stages, taken by the window on its next sync.
    staged: RefCell<Option<crate::scenarios::Staged>>,
    account_root: crate::accounts::AccountRoot,
    wallpaper_status: ui::LocalState<Option<String>>,
    areas: crate::area::Areas,
    keyboard_map_input: ui::KeyboardMapInput,
    area_reads: crate::effects::AreaReads,
    /// The pane shown waits for its area's read.
    pane_waits: Cell<bool>,
    /// The generation of the content column last built.
    content_built: Cell<u64>,
    /// The language it was built in ([`ui::i18n::I18nManager::revision`]).
    content_language: Cell<u64>,
    /// The Keyboard pane was built without its layout list.
    keyboard_partial: Cell<bool>,
    pane_scroll: RefCell<HashMap<Category, ScrollOffset>>,
    /// What the keyboard drawn on the Keyboard pane shows, as the window
    /// last read it; `None` while no keyboard is drawn.
    keyboard_shown: RefCell<Option<serde_json::Value>>,
    /// What the Keyboard pane's add-source picker shows, as the window
    /// last read it; `None` while no picker is drawn.
    input_picker_shown: RefCell<Option<serde_json::Value>>,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        Self::with_account_root(crate::accounts::AccountRoot::live())
    }

    /// The app over the private profile at `config_root`: its accounts
    /// and mutation journal.
    #[must_use]
    pub fn with_config_root(config_root: impl Into<std::path::PathBuf>) -> Self {
        Self::with_account_root(crate::accounts::AccountRoot::at(config_root))
    }

    fn with_account_root(account_root: crate::accounts::AccountRoot) -> Self {
        account_root.recover_mutation_or_warn();
        let mut app = Self {
            me: Weak::new(),
            selected: Cell::new(Category::DEFAULT),
            area_reads: crate::effects::AreaReads::of(false),
            storage: ui::storage::AppStorage::for_app(
                <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID,
            ),
            scene: None,
            devices: false,
            radios: crate::radios::Radios::unread(),
            pages: crate::pages::PanePages::new(),
            secrets: crate::secrets::SecretAsks::new(),
            provider_admin: ui::models::ProviderAdministration::default(),
            linked: ui::linked_accounts::LinkedAccounts::default(),
            sections: crate::sections::Sections::default(),
            sidebar_selection: ui::LocalState::new(None),
            setting_request: ui::LocalState::new(None),
            staged: RefCell::new(None),
            account_root,
            wallpaper_status: ui::LocalState::new(None),
            areas: crate::area::Areas::default(),
            keyboard_map_input: ui::KeyboardMapInput::default(),
            pane_waits: Cell::new(false),
            content_built: Cell::new(0),
            content_language: Cell::new(ui::i18n::I18nManager::revision()),
            keyboard_partial: Cell::new(false),
            pane_scroll: RefCell::default(),
            keyboard_shown: RefCell::new(None),
            input_picker_shown: RefCell::new(None),
        };
        app.areas = crate::area::Areas::of(&app.area_context());
        app
    }

    /// Attach the app to the view `cx`: the settings document and the
    /// sections' actions it holds, what it lends (the provider
    /// administration, the person's linked accounts), the stand-ins a
    /// capture stages, and the person's accounts.
    pub fn attach(
        &mut self,
        cx: &ui::framework::ViewCx<crate::resource::Settings>,
        accounts: Option<ui::framework::PersonRecord<accounts::person::PersonAccounts>>,
    ) {
        let launcher_admin = crate::take_launcher_provider_admin();
        self.provider_admin = if cx.mode().is_live() {
            launcher_admin
        } else {
            ui::models::ProviderAdministration::default()
        };
        while let Some(loan) = cx.take_lent::<ui::models::ProviderAdministration>() {
            self.provider_admin = loan;
        }
        self.linked = ui::linked_accounts::LinkedAccounts::default();
        while let Some(loan) = cx.take_lent::<ui::linked_accounts::LinkedAccounts>() {
            self.linked = loan;
        }
        self.scene = cx.fixture::<crate::scenarios::CaptureScene>();
        if let Some(state) = self
            .scene
            .as_ref()
            .and_then(|scene| scene.linked_account.clone())
        {
            self.linked = ui::linked_accounts::LinkedAccounts::new(Rc::new(
                crate::scenarios::CaptureBridge(state),
            ));
        }
        self.storage = cx.storage().clone();
        self.devices = cx.mode().is_live() && !cfg!(test);
        if self.devices {
            self.radios = crate::radios::Radios::of_device();
        }
        self.sections = crate::sections::Sections::of(cx);
        if let Some(root) = self.scene().and_then(|scene| scene.account_root.clone()) {
            self.account_root = crate::accounts::AccountRoot::at(root);
        } else if let Some(record) = accounts {
            self.account_root = self.account_root.clone().with_person(record);
            if self.devices
                && let Err(error) = self
                    .account_root
                    .take_in_device_accounts()
                    .and_then(|()| self.account_root.project())
            {
                tracing::warn!(%error, "the device's account file could not follow the person's accounts");
            }
        }
        let context = self.area_context();
        self.areas = crate::area::Areas::of(&context);
        #[cfg(test)]
        self.areas.take_replacements(&context);
        self.area_reads = crate::effects::AreaReads::of(self.devices);
    }

    /// What the areas are made in: the parts of this app they keep.
    #[must_use]
    pub fn area_context(&self) -> crate::area::AreaContext {
        crate::area::AreaContext {
            storage: self.storage.clone(),
            scene: self.scene.clone(),
            devices: self.devices,
            radios: self.radios.clone(),
            pages: self.pages.clone(),
            secrets: self.secrets.clone(),
            provider_admin: self.provider_admin.clone(),
            linked: self.linked.clone(),
            sections: self.sections.clone(),
            account_root: self.account_root.clone(),
        }
    }

    /// The app, shared by the window, its effects and its rows.
    #[must_use]
    pub fn into_shared(self) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            ..self
        })
    }

    /// The pane the window shows.
    pub fn selected(&self) -> Category {
        self.selected.get()
    }

    /// The window shows `category` now.
    pub(crate) fn show(&self, category: Category) {
        if category != Category::Keyboard {
            self.keyboard_map_input.clear();
        }
        if self.selected.replace(category) != category {
            self.pages.clear();
        }
    }

    /// Ask the window to show `category`'s pane.
    pub fn request_pane(&self, category: Category) {
        self.sidebar_selection.set(Some(category));
    }

    /// Ask the window to show `setting`: its pane, scrolled to it, its
    /// control taking the keyboard.
    ///
    /// # Errors
    /// Settings has no such setting.
    pub fn request_setting(&self, setting: &str) -> Result<(), String> {
        let (category, setting) = setting_and_pane(setting)
            .ok_or_else(|| format!("Settings has no setting `{setting}`"))?;
        self.setting_request.set(Some(setting));
        self.sidebar_selection.set(Some(category));
        Ok(())
    }

    /// What a search hit or an agent's `open` runs: the pane or the setting
    /// it names is shown.
    pub fn opener(&self) -> impl FnMut(&str) + 'static {
        let selection = self.sidebar_selection.clone();
        let setting_request = self.setting_request.clone();
        move |anchor| {
            if let Some(category) = crate::effects::pane_of_row(anchor) {
                selection.set(Some(category));
            } else if let Some((category, setting)) = anchor
                .strip_prefix(crate::effects::SETTING_ANCHOR)
                .and_then(setting_and_pane)
            {
                setting_request.set(Some(setting));
                selection.set(Some(category));
            }
        }
    }

    /// The pane asked for since the window last looked.
    pub(crate) fn take_requested_pane(&self) -> Option<Category> {
        let requested = self.sidebar_selection.with(|request| *request)?;
        self.sidebar_selection.set(None);
        Some(requested)
    }

    /// Whether a pane is asked for that the window has not shown yet.
    pub(crate) fn pane_requested(&self) -> bool {
        self.sidebar_selection.with(Option::is_some)
    }

    /// The setting asked to be shown since the window last looked.
    pub(crate) fn take_requested_setting(&self) -> Option<&'static settings_store::index::Setting> {
        let requested = self.setting_request.with(|request| *request)?;
        self.setting_request.set(None);
        Some(requested)
    }

    /// Stage `staged` in the window, on its next sync.
    pub fn stage(&self, staged: crate::scenarios::Staged) {
        *self.staged.borrow_mut() = Some(staged);
    }

    /// The scenario staged since the window last looked.
    pub(crate) fn take_staged(&self) -> Option<crate::scenarios::Staged> {
        self.staged.borrow_mut().take()
    }

    pub fn set_browser_identity_enabled(
        &self,
        account: String,
        enabled: bool,
    ) -> Result<(), String> {
        self.account_root
            .set_browser_identity_enabled(account, enabled)
    }

    pub fn remove_browser_identity(&self, account: String) -> Result<(), String> {
        self.account_root.remove_browser_identity(account)
    }

    /// Where the person's accounts and their secrets are kept for this
    /// window.
    pub(crate) fn account_root(&self) -> &crate::accounts::AccountRoot {
        &self.account_root
    }

    pub fn publish_browser_identity(&self, origin: String) -> Result<(), String> {
        let identity =
            ui::local_identity::bound().ok_or("Sign in before publishing this identity")?;
        let root = hex::encode(identity.group_public_key);
        self.account_root
            .publish_browser_identity(root, origin, identity.display_name)
    }

    /// Whether the window publishes `area`'s rows now: it has read, or it
    /// is the pane shown.
    pub fn publishes_area(&self, area: &crate::effects::LiveArea) -> bool {
        (area.is_open)(self)
            || (!self.area_reads.deferred()
                && area
                    .background
                    .as_ref()
                    .is_none_or(|read| (read.finished)(self)))
            || (area.pane == self.selected() && area.background.is_none())
    }

    /// Whether the pane shown waits for its area's background read.
    pub(crate) fn pane_reading(&self) -> bool {
        if !self.area_reads.deferred() {
            return false;
        }
        let selected = self.selected();
        crate::area::registry()
            .live_areas
            .iter()
            .filter(|area| area.pane == selected && !(area.is_open)(self))
            .filter_map(|area| area.background.as_ref())
            .any(|read| {
                let _ = (read.start)(self);
                !(read.finished)(self)
            })
    }

    pub(crate) fn read_areas_now(&self) {
        self.area_reads.wake();
    }

    /// Read on: the next area, or once every area has read, warm them.
    pub(crate) fn read_on(&self) {
        match self.area_reads.step(self) {
            crate::effects::Step::Read => self.area_reads.wake(),
            crate::effects::Step::Waiting => {}
            crate::effects::Step::Done => {
                for area in self.areas.iter() {
                    area.warm();
                }
            }
        }
    }

    pub(crate) fn area_reads(&self) -> &crate::effects::AreaReads {
        &self.area_reads
    }

    pub(crate) fn pane_waits(&self) -> &Cell<bool> {
        &self.pane_waits
    }

    /// The generation of the content column last built.
    pub(crate) fn content_built(&self) -> &Cell<u64> {
        &self.content_built
    }

    /// The language the content column last built was built in.
    pub(crate) fn content_language(&self) -> &Cell<u64> {
        &self.content_language
    }

    /// Whether the pane shown is not yet built as it will show: it waits
    /// for its area's read, or shows an area's first reading of the device
    /// as on its way.
    pub(crate) fn pane_settling(&self) -> bool {
        self.pane_waits.get() || self.areas.iter().any(crate::area::PaneArea::first_read)
    }

    pub(crate) fn keyboard_partial(&self) -> &Cell<bool> {
        &self.keyboard_partial
    }

    /// Where `category`'s pane is scrolled to, kept while the window is open.
    pub(crate) fn scroll_offset(&self, category: Category) -> ScrollOffset {
        Rc::clone(self.pane_scroll.borrow_mut().entry(category).or_default())
    }

    /// Forget where `category`'s pane was scrolled to: a page drilled into
    /// opens at its top.
    pub(crate) fn forget_scroll(&self, category: Category) {
        self.pane_scroll.borrow_mut().remove(&category);
    }

    pub fn wallpaper_status(&self) -> &ui::LocalState<Option<String>> {
        &self.wallpaper_status
    }

    pub fn areas(&self) -> &crate::area::Areas {
        &self.areas
    }

    pub fn keyboard_map_input(&self) -> ui::KeyboardMapInput {
        self.keyboard_map_input.clone()
    }

    /// What the keyboard drawn on the Keyboard pane shows: the source,
    /// the key inspected, the keys held. Unavailable while none is drawn.
    ///
    /// # Errors
    /// The drawn keyboard could not be read.
    pub fn keyboard_inspection(&self) -> Result<serde_json::Value, String> {
        Ok(self.keyboard_shown.borrow().clone().unwrap_or_else(|| {
            serde_json::json!({
                "available": false, "page_visible": false, "foreground": false,
                "mode": null, "source": null, "model": null, "inspected_key": null,
                "selected_symbols": [], "native_layout_available": false,
                "native_group": null, "native_modifiers": null, "pressed_physical_keys": [],
            })
        }))
    }

    /// The window read the keyboard it draws: `shown`, or none drawn.
    pub(crate) fn keyboard_drawn(&self, shown: Option<serde_json::Value>) {
        *self.keyboard_shown.borrow_mut() = shown;
    }

    /// What the Keyboard pane's add-source picker shows; `None` while none
    /// is drawn.
    pub(crate) fn input_picker(&self) -> Option<serde_json::Value> {
        self.input_picker_shown.borrow().clone()
    }

    /// The window read the add-source picker it draws: `shown`, or none.
    pub(crate) fn input_picker_drawn(&self, shown: Option<serde_json::Value>) {
        *self.input_picker_shown.borrow_mut() = shown;
    }

    /// The app's storage: its settings document, files and secrets.
    #[must_use]
    pub fn storage(&self) -> &ui::storage::AppStorage {
        &self.storage
    }

    /// The stand-ins a screenshot shows; `None` in a live window.
    #[must_use]
    pub fn scene(&self) -> Option<&crate::scenarios::CaptureScene> {
        self.scene.as_deref()
    }

    /// Whether the areas reach this device's hardware and services.
    #[must_use]
    pub fn reaches_devices(&self) -> bool {
        self.devices
    }

    /// The radios this device has, read off the UI thread.
    #[must_use]
    pub fn radios(&self) -> &crate::radios::Radios {
        &self.radios
    }

    /// The pages the panes drilled into ([`crate::pages`]).
    #[must_use]
    pub fn pages(&self) -> &crate::pages::PanePages {
        &self.pages
    }

    /// The password prompt the window shows ([`crate::secrets`]).
    #[must_use]
    pub fn secrets(&self) -> &crate::secrets::SecretAsks {
        &self.secrets
    }

    /// The administration of the agent providers the launcher granted.
    #[must_use]
    pub fn provider_admin(&self) -> &ui::models::ProviderAdministration {
        &self.provider_admin
    }

    /// The person's linked accounts, when the host lent them.
    #[must_use]
    pub fn linked(&self) -> &ui::linked_accounts::LinkedAccounts {
        &self.linked
    }

    /// The settings sections' actions ([`crate::sections`]).
    #[must_use]
    pub fn sections(&self) -> &crate::sections::Sections {
        &self.sections
    }

    /// Run blocking `work` off the UI thread for an effect, then `then` with
    /// its result and this app on the UI thread.
    pub fn job<T: Send + 'static>(
        &self,
        cx: &mut crate::device_read::Cx,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&SettingsApp, T, &mut crate::device_read::Cx) -> ui::framework::EffectAnswer
        + 'static,
    ) -> ui::framework::Effected {
        crate::device_read::job(cx, self.me.clone(), work, then)
    }
}

pub(crate) fn setting_and_pane(
    id: &str,
) -> Option<(Category, &'static settings_store::index::Setting)> {
    let setting = settings_store::index::setting(id)?;
    Some((Category::from_slug(setting.page)?, setting))
}
