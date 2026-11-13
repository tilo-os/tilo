//! `SettingsApp` — what the Settings window shares with its view: the areas the
//! panes read, the settings document and the sections' actions, the pane shown
//! and what asks for another.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::categories::Category;

pub(crate) type ScrollOffset = Rc<Cell<(f32, f32)>>;

pub struct SettingsApp {
    me: Weak<SettingsApp>,
    selected: Cell<Category>,
    storage: ui::storage::AppStorage,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    devices: bool,
    /// The radios this device has; never read when it reaches no device.
    radios: crate::radios::Radios,
    pages: crate::pages::PanePages,
    secrets: crate::secrets::SecretAsks,
    provider_admin: ui::models::ProviderAdministration,
    linked: ui::linked_accounts::LinkedAccounts,
    sections: crate::sections::Sections,
    sidebar_selection: ui::LocalState<Option<Category>>,
    setting_request: ui::LocalState<Option<&'static settings_store::index::Setting>>,
    staged: RefCell<Option<crate::scenarios::Staged>>,
    account_root: crate::accounts::AccountRoot,
    wallpaper_status: ui::LocalState<Option<String>>,
    areas: crate::area::Areas,
    keyboard_map_input: ui::KeyboardMapInput,
    area_reads: crate::effects::AreaReads,
    pane_waits: ui::LocalState<bool>,
    content_built: Cell<u64>,
    content_language: ui::LocalState<u64>,
    keyboard_partial: ui::LocalState<bool>,
    pane_scroll: RefCell<HashMap<Category, ScrollOffset>>,
    keyboard_shown: RefCell<Option<serde_json::Value>>,
    #[cfg(feature = "tilo-os")]
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
            pane_waits: ui::LocalState::new(false),
            content_built: Cell::new(0),
            content_language: ui::LocalState::new(ui::i18n::I18nManager::revision()),
            keyboard_partial: ui::LocalState::new(false),
            pane_scroll: RefCell::default(),
            keyboard_shown: RefCell::new(None),
            #[cfg(feature = "tilo-os")]
            input_picker_shown: RefCell::new(None),
        };
        app.areas = crate::area::Areas::of(&app.area_context());
        app
    }

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
        self.devices = cx.mode().is_live() && !cfg!(test) && cfg!(feature = "tilo-os");
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

    #[must_use]
    pub fn into_shared(self) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            ..self
        })
    }

    pub fn selected(&self) -> Category {
        self.selected.get()
    }

    pub(crate) fn show(&self, category: Category) {
        if category != Category::Keyboard {
            self.keyboard_map_input.clear();
        }
        if self.selected.replace(category) != category {
            self.pages.clear();
        }
    }

    pub fn request_pane(&self, category: Category) {
        self.sidebar_selection.set(Some(category));
    }

    pub fn request_setting(&self, setting: &str) -> Result<(), String> {
        let (category, setting) = setting_and_pane(setting)
            .ok_or_else(|| format!("Settings has no setting `{setting}`"))?;
        self.setting_request.set(Some(setting));
        self.sidebar_selection.set(Some(category));
        Ok(())
    }

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

    pub(crate) fn take_requested_pane(&self) -> Option<Category> {
        let requested = self.sidebar_selection.with(|request| *request)?;
        self.sidebar_selection.set(None);
        Some(requested)
    }

    /// Whether a pane is asked for that the window has not shown yet.
    pub(crate) fn pane_requested(&self) -> bool {
        self.sidebar_selection.with(Option::is_some)
    }

    pub(crate) fn take_requested_setting(&self) -> Option<&'static settings_store::index::Setting> {
        let requested = self.setting_request.with(|request| *request)?;
        self.setting_request.set(None);
        Some(requested)
    }

    pub fn stage(&self, staged: crate::scenarios::Staged) {
        *self.staged.borrow_mut() = Some(staged);
    }

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

    #[cfg(feature = "tilo-os")]
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

    pub fn publishes_area(&self, area: &crate::effects::LiveArea) -> bool {
        (area.is_open)(self)
            || (!self.area_reads.deferred()
                && area
                    .background
                    .as_ref()
                    .is_none_or(|read| (read.finished)(self)))
            || (area.pane == self.selected() && area.background.is_none())
    }

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

    pub(crate) fn pane_waits(&self) -> &ui::LocalState<bool> {
        &self.pane_waits
    }

    pub(crate) fn content_built(&self) -> &Cell<u64> {
        &self.content_built
    }

    pub(crate) fn content_language(&self) -> &ui::LocalState<u64> {
        &self.content_language
    }

    /// Whether the pane shown is not yet built as it will show: it waits
    /// for its area's read, or shows an area's first reading of the device
    /// as on its way.
    pub(crate) fn pane_settling(&self) -> bool {
        self.pane_waits.get() || self.areas.iter().any(crate::area::PaneArea::first_read)
    }

    pub(crate) fn keyboard_partial(&self) -> &ui::LocalState<bool> {
        &self.keyboard_partial
    }

    pub(crate) fn scroll_offset(&self, category: Category) -> ScrollOffset {
        Rc::clone(self.pane_scroll.borrow_mut().entry(category).or_default())
    }

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

    #[cfg(feature = "tilo-os")]
    pub(crate) fn keyboard_drawn(&self, shown: Option<serde_json::Value>) {
        *self.keyboard_shown.borrow_mut() = shown;
    }

    #[cfg(feature = "tilo-os")]
    pub(crate) fn input_picker(&self) -> Option<serde_json::Value> {
        self.input_picker_shown.borrow().clone()
    }

    #[cfg(feature = "tilo-os")]
    pub(crate) fn input_picker_drawn(&self, shown: Option<serde_json::Value>) {
        *self.input_picker_shown.borrow_mut() = shown;
    }

    #[must_use]
    pub fn storage(&self) -> &ui::storage::AppStorage {
        &self.storage
    }

    #[must_use]
    pub fn scene(&self) -> Option<&crate::scenarios::CaptureScene> {
        self.scene.as_deref()
    }

    #[must_use]
    pub fn reaches_devices(&self) -> bool {
        self.devices
    }

    #[must_use]
    pub fn radios(&self) -> &crate::radios::Radios {
        &self.radios
    }

    #[must_use]
    pub fn pages(&self) -> &crate::pages::PanePages {
        &self.pages
    }

    #[must_use]
    pub fn secrets(&self) -> &crate::secrets::SecretAsks {
        &self.secrets
    }

    #[must_use]
    pub fn provider_admin(&self) -> &ui::models::ProviderAdministration {
        &self.provider_admin
    }

    #[must_use]
    pub fn linked(&self) -> &ui::linked_accounts::LinkedAccounts {
        &self.linked
    }

    #[must_use]
    pub fn sections(&self) -> &crate::sections::Sections {
        &self.sections
    }

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
