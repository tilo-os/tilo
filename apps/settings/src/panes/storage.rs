//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.
//! Reclaiming space is a [`StorageEffect`] the [`StorageModel`] runs.

mod other_devices;
mod residency;

pub use residency::{DeviceResidency, Fleet};

use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use runtime_fs::storage::{
    CachePolicy, DEFAULT_STORAGE_SOCKET, EvictionReport, EvictionSelection, ResidencySummary,
    StorageClient, SyncStorageSnapshot,
};
use std::cell::RefCell;
use std::rc::Rc;

use provider_settings_ui::bound::SettingRows as _;
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, EffectReply, Effected, ViewCx};
use ui::i18n::ArgValue;
use ui::tilo_resource;
use ui::widget::{Button, Element};

use crate::area::AreaContext;
use crate::device_read::{Landed, Landing};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::replies::Replies;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, note, rows, section, section_with, value_row,
};

crate::effects::effect_family!(StorageEffect => answer);

crate::area::pane_area! {
    StorageArea;
    fn poll(&self) {
        StorageArea::poll(self);
    }
    fn busy(&self) -> bool {
        StorageArea::busy(self)
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&StorageArea> {
    app.areas().find()
}

pub(crate) mod values {
    use super::{StorageModel, area, cache_size, cache_size_shown};
    use crate::i18n::{t, t_args};
    use crate::setting::Follows;
    use crate::settings_app::SettingsApp;

    fn read<V>(app: &SettingsApp, read: impl FnOnce(&StorageModel) -> Option<V>) -> Option<V> {
        let model = area(app)?.model.get()?;
        let model = model.borrow();
        read(&model)
    }

    pub(crate) fn cache(app: &SettingsApp, _: &Follows) -> Option<String> {
        read(app, |model| {
            cache_size(model.cache_policy?.budget_bytes).map(cache_size_shown)
        })
    }

    pub(crate) fn clean(app: &SettingsApp, _: &Follows) -> Option<String> {
        read(app, |model| {
            let scan = model.scan.as_ref()?;
            Some(match &scan.cleanup {
                Some(preview) if !preview.items.is_empty() => t_args(
                    "settings-storage-reclaimable",
                    &[("size", ui::format::size(preview.total_bytes).into())],
                ),
                _ => t("settings-storage-clean-nothing"),
            })
        })
    }
}

fn category_label(category: StorageCategory) -> String {
    t(match category {
        StorageCategory::InstalledApps => "settings-storage-cat-apps",
        StorageCategory::AppData => "settings-storage-cat-data",
        StorageCategory::Cache => "settings-storage-cat-cache",
        StorageCategory::SyncedFiles => "settings-storage-cat-synced",
    })
}

const CACHE_SIZES: &[&str] = &["Automatic", "5 GB", "10 GB", "25 GB", "50 GB", "100 GB"];

fn cache_budget(size: &str) -> Result<Option<u64>, String> {
    if size == "Automatic" {
        return Ok(None);
    }
    size.strip_suffix(" GB")
        .filter(|_| CACHE_SIZES.contains(&size))
        .and_then(|gigabytes| gigabytes.parse::<u64>().ok())
        .map(|gigabytes| Some(gigabytes.saturating_mul(1_000_000_000)))
        .ok_or_else(|| {
            t_args(
                "settings-storage-cache-size-unknown",
                &[("size", size.into())],
            )
        })
}

fn cache_size(budget: Option<u64>) -> Option<&'static str> {
    CACHE_SIZES
        .iter()
        .copied()
        .find(|size| cache_budget(size).ok() == Some(budget))
}

fn cache_size_shown(size: &str) -> String {
    match size.strip_suffix(" GB") {
        Some(gigabytes) => t_args(
            "settings-storage-cache-size-gb",
            &[("size", gigabytes.into())],
        ),
        None => t("settings-storage-cache-size-auto"),
    }
}

fn cache_size_from_label(label: &str) -> String {
    CACHE_SIZES
        .iter()
        .find(|size| **size == label || cache_size_shown(size) == label)
        .map_or_else(|| label.to_string(), |size| (*size).to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum StorageEffect {
    /// Remove the apps' caches.
    #[effect(job, destructive, returns)]
    CleanUpCaches,
    /// Free the local copies of synced files that are safe on the server.
    #[effect(job, destructive, returns)]
    FreeSyncedFiles,
    /// Open Disks.
    #[effect(job, returns)]
    #[effect(label = "settings-storage-open-disks")]
    OpenDisks,
    /// Open Backups.
    #[effect(job, returns)]
    #[effect(label = "settings-storage-open-backups")]
    OpenBackups,
    /// Fetch the workspace files you are likely to open next before you
    /// open them, or only when opened.
    #[effect(job, idempotent, returns)]
    SetPrefetch { on: bool },
    /// Set how much space fetched workspace files may use on this device.
    #[effect(job, idempotent, returns)]
    SetCacheSize {
        #[param("The space to keep", choices = CACHE_SIZES.iter().copied())]
        size: String,
    },
    /// What this device holds of each workspace.
    ///
    /// Answers, per workspace, the bytes kept here by choice, also held by
    /// another device, held only here, and cache. Names no file.
    #[effect(job, returns, idempotent, kind = "read")]
    StorageSummary,
    /// Free a workspace's cache.
    ///
    /// Drops what this device fetched from another one; it downloads again
    /// when opened.
    #[effect(job, returns)]
    FreeCache {
        #[param("The workspace, as the storage summary names it")]
        workspace: String,
    },
    /// Keep a whole workspace on this device, or stop keeping it.
    #[effect(job, idempotent, returns)]
    KeepHere {
        #[param("The workspace, as the storage summary names it")]
        workspace: String,
        keep: bool,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: StorageEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = crate::panes::storage::area(app).ok_or_else(crate::area::missing)?;
    match effect {
        StorageEffect::CleanUpCaches => {
            let preview = area.model().borrow_mut().begin_clean_up()?;
            let device = area.device();
            Ok(app.job(
                cx,
                move || -> Result<StorageScan, String> {
                    device.clean_up(&preview)?;
                    Ok(device.scan())
                },
                |app: &SettingsApp, cleaned: Result<StorageScan, String>, _| {
                    crate::panes::storage::area(app)
                        .ok_or_else(crate::area::missing)?
                        .model()
                        .borrow_mut()
                        .cleaned(cleaned)
                },
            ))
        }
        StorageEffect::FreeSyncedFiles => {
            let reply = cx.reply();
            area.model().borrow_mut().free_synced_files(reply);
            Ok(Effected::Pending)
        }
        StorageEffect::SetPrefetch { on } => {
            let reply = cx.reply();
            area.model().borrow_mut().set_policy(
                StorageEffect::SET_PREFETCH.id,
                reply,
                move |policy| {
                    policy.prefetch = on;
                    Ok(())
                },
            );
            Ok(Effected::Pending)
        }
        StorageEffect::SetCacheSize { size } => {
            let budget = cache_budget(&size)?;
            let reply = cx.reply();
            area.model().borrow_mut().set_policy(
                StorageEffect::SET_CACHE_SIZE.id,
                reply,
                move |policy| {
                    policy.budget_bytes = budget;
                    Ok(())
                },
            );
            Ok(Effected::Pending)
        }
        StorageEffect::StorageSummary => {
            let reply = cx.reply();
            area.model().borrow_mut().summarize(reply);
            Ok(Effected::Pending)
        }
        StorageEffect::FreeCache { workspace } => {
            let reply = cx.reply();
            area.model().borrow_mut().free_cache(workspace, reply);
            Ok(Effected::Pending)
        }
        StorageEffect::KeepHere { workspace, keep } => {
            let reply = cx.reply();
            area.model().borrow_mut().keep_here(workspace, keep, reply);
            Ok(Effected::Pending)
        }
        StorageEffect::OpenDisks => Ok(open_tool(
            app,
            area,
            cx,
            ToolIntent::Disks {
                route: DisksRoute::Overview,
            },
            "settings-storage-opened-disks",
        )),
        StorageEffect::OpenBackups => Ok(open_tool(
            app,
            area,
            cx,
            ToolIntent::Backups {
                route: BackupsRoute::Overview,
            },
            "settings-storage-opened-backups",
        )),
    }
}

fn open_tool(
    app: &SettingsApp,
    area: &StorageArea,
    cx: &mut EffectCx<Settings>,
    intent: ToolIntent,
    opened: &'static str,
) -> Effected {
    let device = area.device();
    let on_behalf_of = ui::invocation_scope::delivered_invocation().map(|id| id.0.into_bytes());
    app.job(
        cx,
        move || device.open_tool(intent, on_behalf_of),
        move |_: &SettingsApp, result: Result<(), String>, _| {
            result.map(|()| Effected::value(t(opened)))
        },
    )
}

fn build_with_drives(storage: Element, drives: Element) -> Element {
    Box::new(
        ui::widget::Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .push(storage)
            .push(drives),
    )
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StorageScan {
    pub(crate) snapshot: StorageSnapshot,
    pub(crate) cleanup: Option<CleanupPreview>,
    pub(crate) capacity: Option<os_actions::DiskCapacity>,
}

impl StorageScan {
    fn of(roots: &StorageRoots, disk: Option<&std::path::Path>) -> Self {
        Self {
            snapshot: desktop_policy::scan_storage(roots),
            cleanup: roots
                .app_cache
                .as_deref()
                .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok()),
            capacity: disk.and_then(|path| os_actions::disk_capacity(path).ok()),
        }
    }

    fn unavailable() -> Self {
        Self {
            snapshot: StorageSnapshot {
                entries: Vec::new(),
                unavailable: vec![
                    StorageCategory::InstalledApps,
                    StorageCategory::AppData,
                    StorageCategory::Cache,
                ],
            },
            cleanup: None,
            capacity: None,
        }
    }
}

/// What Storage asks of the device. Each call may take long — a walk of
/// the sync replica, a round trip to the shell — so it runs off the UI
/// thread.
pub(crate) trait StorageDevice: Send + Sync {
    fn scan(&self) -> StorageScan;
    fn clean_up(&self, preview: &CleanupPreview) -> Result<(), String>;
    fn open_tool(&self, intent: ToolIntent, on_behalf_of: Option<[u8; 16]>) -> Result<(), String>;
}

struct DeviceStorage {
    roots: StorageRoots,
    disk: Option<std::path::PathBuf>,
    reaches_shell: bool,
}

impl DeviceStorage {
    fn of(devices: bool) -> Self {
        if devices {
            Self {
                roots: StorageRoots::discover(),
                disk: std::env::var_os("HOME").map(std::path::PathBuf::from),
                reaches_shell: true,
            }
        } else {
            Self {
                roots: StorageRoots {
                    app_payloads: None,
                    room_data: None,
                    app_cache: None,
                    sync_replica: None,
                },
                disk: None,
                reaches_shell: false,
            }
        }
    }
}

impl StorageDevice for DeviceStorage {
    fn scan(&self) -> StorageScan {
        crate::device_read::off_ui_thread();
        StorageScan::of(&self.roots, self.disk.as_deref())
    }

    fn clean_up(&self, preview: &CleanupPreview) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let selected: Vec<usize> = (0..preview.items.len()).collect();
        desktop_policy::apply_cleanup(preview, &selected)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn open_tool(&self, intent: ToolIntent, on_behalf_of: Option<[u8; 16]>) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        if !self.reaches_shell {
            return Err("only a window someone uses opens a storage tool".into());
        }
        os_actions::open_tool(intent, on_behalf_of)
    }
}

pub struct StorageArea {
    devices: bool,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    device: std::cell::OnceCell<std::sync::Arc<dyn StorageDevice>>,
    model: std::cell::OnceCell<Rc<RefCell<StorageModel>>>,
    remote: ui::RemoteActions,
    shown: LocalState<u64>,
}

impl StorageArea {
    pub fn new(cx: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            devices: cx.devices,
            scene: cx.scene.clone(),
            device: std::cell::OnceCell::new(),
            model: std::cell::OnceCell::new(),
            remote,
            shown: LocalState::new(0),
        }
    }

    fn device(&self) -> std::sync::Arc<dyn StorageDevice> {
        std::sync::Arc::clone(
            self.device
                .get_or_init(|| std::sync::Arc::new(DeviceStorage::of(self.devices))),
        )
    }

    #[cfg(test)]
    pub(crate) fn use_device(&self, device: std::sync::Arc<dyn StorageDevice>) {
        assert!(
            self.device.set(device).is_ok(),
            "the storage device is chosen before the area is used"
        );
    }

    pub fn model(&self) -> &Rc<RefCell<StorageModel>> {
        self.model.get_or_init(|| {
            let mut model = StorageModel::over(self.devices, self.device());
            if self.devices {
                model.refresh_sync();
                model.ask_other_devices(other_devices::OtherDevices::new(self.remote.clone()));
            } else {
                for device in self
                    .scene
                    .as_deref()
                    .map(|scene| scene.storage_devices.clone())
                    .unwrap_or_default()
                {
                    model.fleet.adopt(device);
                }
                model.capacity_stand_in = self
                    .scene
                    .as_deref()
                    .and_then(|scene| scene.storage_capacity);
            }
            Rc::new(RefCell::new(model))
        })
    }

    pub(crate) fn poll(&self) {
        let Some(model) = self.model.get() else {
            return;
        };
        let Ok(mut model) = model.try_borrow_mut() else {
            return;
        };
        model.take_in();
        if std::mem::take(&mut model.changed) {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn busy(&self) -> bool {
        self.model
            .get()
            .and_then(|model| model.try_borrow().ok())
            .is_some_and(|model| model.busy())
    }
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    held.poll();
    let storage = cx.switch(&held.shown, move |cx, _| model.borrow().page(cx));
    let drives = crate::panes::drives::view(cx, app)
        .unwrap_or_else(|| provider_settings_ui::widgets::page(Vec::new()));
    Some(build_with_drives(storage, drives))
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: whether it asks the device, what runs, what changed"
)]
pub struct StorageModel {
    devices: bool,
    scan: Option<StorageScan>,
    scanning: Option<Landing>,
    cleaning: bool,
    sync_snapshot: Option<SyncStorageSnapshot>,
    sync_message: Option<String>,
    sync_result: Option<Landing>,
    snapshot_result: Option<Landing>,
    cache_policy: Option<CachePolicy>,
    policy_result: Option<PolicyAnswer>,
    fleet: Fleet,
    capacity_stand_in: Option<os_actions::DiskCapacity>,
    others: Option<other_devices::OtherDevices>,
    residency_job: Option<(&'static str, Landing)>,
    summary_waiting: bool,
    replies: Replies,
    changed: bool,
}

struct SyncRead {
    snapshot: Result<SyncStorageSnapshot, String>,
    residency: Option<DeviceResidency>,
}

impl SyncRead {
    fn now() -> Self {
        crate::device_read::off_ui_thread();
        let mut client = match StorageClient::connect(DEFAULT_STORAGE_SOCKET) {
            Ok(client) => client,
            Err(error) => {
                return Self {
                    snapshot: Err(error.to_string()),
                    residency: None,
                };
            }
        };
        let snapshot = client.snapshot().map_err(|error| error.to_string());
        let residency = client.residency().ok().map(|summary| DeviceResidency {
            key: String::new(),
            label: String::new(),
            current: true,
            summary,
        });
        Self {
            snapshot,
            residency,
        }
    }
}

impl StorageModel {
    fn over(devices: bool, device: std::sync::Arc<dyn StorageDevice>) -> Self {
        Self {
            devices,
            scan: None,
            scanning: Some(Landing::start(move || device.scan())),
            cleaning: false,
            sync_snapshot: None,
            sync_message: None,
            sync_result: None,
            snapshot_result: None,
            cache_policy: None,
            policy_result: None,
            fleet: Fleet::default(),
            capacity_stand_in: None,
            others: None,
            residency_job: None,
            summary_waiting: false,
            replies: Replies::default(),
            changed: false,
        }
    }

    fn ask_other_devices(&mut self, mut others: other_devices::OtherDevices) {
        others.refresh();
        self.others = Some(others);
    }

    fn poll_other_devices(&mut self) -> bool {
        let Some(others) = self.others.as_mut() else {
            return false;
        };
        let arrived = others.poll();
        let changed = arrived.listed.is_some() || !arrived.summaries.is_empty();
        if let Some(listed) = &arrived.listed {
            self.fleet.keep_listed(listed);
        }
        for device in arrived.summaries {
            self.fleet.adopt(device);
        }
        changed
    }

    fn refresh_sync(&mut self) {
        if let Some(others) = self.others.as_mut() {
            others.refresh();
        }
        let receiver = Landing::start(SyncRead::now);
        if self.sync_snapshot.is_none() {
            self.sync_message = Some(t("settings-storage-loading"));
        }
        self.snapshot_result = Some(receiver);
        if self.policy_result.is_none() {
            self.change_policy(None, |_| Ok(()));
        }
    }

    fn change_policy(
        &mut self,
        effect: Option<&'static str>,
        change: impl FnOnce(&mut CachePolicy) -> Result<(), String> + Send + 'static,
    ) {
        let read_only = effect.is_none();
        let receiver = Landing::start(move || -> Result<CachePolicy, String> {
            StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                .and_then(|mut client| {
                    let mut policy = client.cache_policy()?;
                    if read_only {
                        return Ok(Ok(policy));
                    }
                    if let Err(error) = change(&mut policy) {
                        return Ok(Err(error));
                    }
                    client.set_cache_policy(policy).map(Ok)
                })
                .map_err(|error| error.to_string())
                .and_then(|changed| changed)
        });
        self.policy_result = Some(PolicyAnswer { effect, receiver });
    }

    fn set_policy(
        &mut self,
        id: &'static str,
        reply: EffectReply,
        change: impl FnOnce(&mut CachePolicy) -> Result<(), String> + Send + 'static,
    ) {
        self.wait(id, reply);
        self.change_policy(Some(id), change);
    }

    fn begin_clean_up(&mut self) -> Result<CleanupPreview, String> {
        if self.cleaning {
            return Err(t("settings-storage-cleaning"));
        }
        let Some(scan) = &self.scan else {
            return Err(t("settings-pane-reading"));
        };
        let preview = scan
            .cleanup
            .clone()
            .ok_or_else(|| t("settings-storage-no-cache"))?;
        self.cleaning = true;
        self.changed = true;
        Ok(preview)
    }

    fn cleaned(&mut self, cleaned: Result<StorageScan, String>) -> EffectAnswer {
        self.cleaning = false;
        self.changed = true;
        match cleaned {
            Ok(scan) => {
                self.scan = Some(scan);
                Ok(Effected::value(t("settings-storage-cleaned")))
            }
            Err(error) => Err(t_args(
                "settings-storage-clean-failed",
                &[("error", error.into())],
            )),
        }
    }

    fn wait(&mut self, id: &'static str, reply: EffectReply) {
        self.replies.wait(id, reply);
        self.changed = true;
    }

    fn summarize(&mut self, reply: EffectReply) {
        self.wait(StorageEffect::STORAGE_SUMMARY.id, reply);
        if !self.devices {
            let summary = self
                .fleet
                .this_device()
                .map(|device| device.summary.clone())
                .unwrap_or_default();
            self.answer_summary(&summary);
            return;
        }
        self.summary_waiting = true;
        if self.snapshot_result.is_none() {
            self.refresh_sync();
        }
    }

    fn answer_summary(&mut self, summary: &ResidencySummary) {
        self.summary_waiting = false;
        match serde_json::to_value(summary) {
            Ok(value) => self
                .replies
                .answer(StorageEffect::STORAGE_SUMMARY.id, value),
            Err(error) => self
                .replies
                .fail(StorageEffect::STORAGE_SUMMARY.id, error.to_string()),
        }
    }

    fn free_cache(&mut self, workspace: String, reply: EffectReply) {
        self.run_residency_job(StorageEffect::FREE_CACHE.id, reply, move |client| {
            client.free_cache(&workspace).map(|freed| {
                t_args(
                    "settings-storage-cache-freed",
                    &[
                        ("size", ui::format::size(freed.freed_bytes).into()),
                        ("files", ArgValue::Number(freed.files as f64)),
                    ],
                )
            })
        });
    }

    fn keep_here(&mut self, workspace: String, keep: bool, reply: EffectReply) {
        self.run_residency_job(StorageEffect::KEEP_HERE.id, reply, move |client| {
            client.keep_here(&workspace, keep).map(|()| {
                t(if keep {
                    "settings-storage-now-kept"
                } else {
                    "settings-storage-no-longer-kept"
                })
            })
        });
    }

    fn run_residency_job(
        &mut self,
        id: &'static str,
        reply: EffectReply,
        job: impl FnOnce(&mut StorageClient) -> std::io::Result<String> + Send + 'static,
    ) {
        self.wait(id, reply);
        if self.residency_job.is_some() {
            self.replies.fail(id, t("settings-storage-residency-busy"));
            return;
        }
        if !self.devices {
            self.replies
                .fail(id, t("settings-storage-accounting-unavailable"));
            return;
        }
        let receiver = Landing::start(move || -> Result<String, String> {
            StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                .and_then(|mut client| job(&mut client))
                .map_err(|error| error.to_string())
        });
        self.residency_job = Some((id, receiver));
    }

    fn free_synced_files(&mut self, reply: EffectReply) {
        self.wait(StorageEffect::FREE_SYNCED_FILES.id, reply);
        if self.sync_result.is_none()
            && let Some(snapshot) = &self.sync_snapshot
        {
            let selected = reclaimable_selections(snapshot);
            let receiver = Landing::start(move || -> Result<EvictionReport, String> {
                StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                    .and_then(|mut client| client.evict(selected))
                    .map_err(|error| error.to_string())
            });
            self.sync_result = Some(receiver);
        } else {
            self.replies.finish(
                StorageEffect::FREE_SYNCED_FILES.id,
                Err(t("settings-storage-no-reclaimable")),
            );
        }
    }

    pub(crate) fn take_in(&mut self) {
        let waiting = (self.snapshot_result.is_some(), self.sync_result.is_some());
        let measured = self.poll_scan();
        let residency_finished = self.poll_residency_job();
        if let Some(receiver) = &self.snapshot_result {
            match receiver.poll::<SyncRead>() {
                Landed::Done(read) => {
                    self.snapshot_result = None;
                    self.take_read(read);
                }
                Landed::Lost => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some(t("settings-storage-service-ended"));
                    if self.summary_waiting {
                        self.summary_waiting = false;
                        self.replies.fail(
                            StorageEffect::STORAGE_SUMMARY.id,
                            t("settings-storage-service-ended"),
                        );
                    }
                }
                Landed::Pending => {}
            }
        }
        if let Some(receiver) = &self.sync_result {
            match receiver.poll::<Result<EvictionReport, String>>() {
                Landed::Done(Ok(report)) => {
                    self.sync_result = None;
                    self.refresh_sync();
                    self.sync_message =
                        Some(if report.queued_files > 0 && report.evicted_files > 0 {
                            t_args(
                                "settings-storage-freed-queued",
                                &[
                                    ("size", ui::format::size(report.freed_bytes).into()),
                                    ("evicted", ArgValue::Number(report.evicted_files as f64)),
                                    ("queued", ArgValue::Number(report.queued_files as f64)),
                                ],
                            )
                        } else if report.queued_files > 0 {
                            t_args(
                                "settings-storage-queued-only",
                                &[("queued", ArgValue::Number(report.queued_files as f64))],
                            )
                        } else {
                            t_args(
                                "settings-storage-freed",
                                &[
                                    ("size", ui::format::size(report.freed_bytes).into()),
                                    ("evicted", ArgValue::Number(report.evicted_files as f64)),
                                ],
                            )
                        });
                    self.replies.finish(
                        StorageEffect::FREE_SYNCED_FILES.id,
                        Ok(t("settings-storage-reclaim-done")),
                    );
                }
                Landed::Done(Err(error)) => {
                    self.sync_result = None;
                    let message = t_args(
                        "settings-storage-nothing-removed",
                        &[("error", error.into())],
                    );
                    self.sync_message = Some(message.clone());
                    self.replies
                        .finish(StorageEffect::FREE_SYNCED_FILES.id, Err(message));
                }
                Landed::Lost => {
                    self.sync_result = None;
                    self.sync_message = Some(t("settings-storage-request-ended"));
                    self.replies.finish(
                        StorageEffect::FREE_SYNCED_FILES.id,
                        Err(t("settings-storage-service-ended")),
                    );
                }
                Landed::Pending => {}
            }
        }
        let policy_changed = self.poll_policy();
        let others_answered = self.poll_other_devices();
        let finished = (waiting.0 && self.snapshot_result.is_none())
            || (waiting.1 && self.sync_result.is_none())
            || residency_finished
            || others_answered;
        self.changed |= finished || measured || policy_changed;
    }

    fn poll_scan(&mut self) -> bool {
        let Some(receiver) = &self.scanning else {
            return false;
        };
        let scan = match receiver.poll::<StorageScan>() {
            Landed::Done(scan) => scan,
            Landed::Lost => StorageScan::unavailable(),
            Landed::Pending => return false,
        };
        self.scan = Some(scan);
        self.scanning = None;
        true
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        let actions = cx.actions();
        let mut sections = vec![self.overview_card()];
        sections.extend(residency::page(
            &actions,
            &self.fleet,
            self.residency_job.is_some(),
        ));
        sections.push(self.synced_card(&actions));
        sections.extend(self.cache_card(cx));
        sections.extend([
            self.apps_card(),
            self.cleanup_card(&actions),
            Self::tools_card(cx),
        ]);
        provider_settings_ui::widgets::page(sections)
    }

    fn overview_card(&self) -> SettingsSection {
        let mut items = Vec::new();
        match self
            .scan
            .as_ref()
            .and_then(|scan| scan.capacity)
            .or(self.capacity_stand_in)
        {
            Some(capacity) => {
                let used = t_args(
                    "settings-storage-capacity-used",
                    &[
                        ("used", ui::format::size(capacity.used_bytes()).into()),
                        ("total", ui::format::size(capacity.total_bytes).into()),
                    ],
                );
                items.push(provider_settings_ui::widgets::block(Box::new(
                    ui::widget::ProgressBar::new(capacity.used_fraction())
                        .level(ui::widget::GaugeThresholds {
                            warning: 0.8,
                            danger: 0.95,
                        })
                        .with_label(used.clone()),
                )));
                items.push(value_row(&t("settings-storage-capacity-used-label"), &used));
                items.push(value_row(
                    &t("settings-storage-capacity-available"),
                    &ui::format::size(capacity.available_bytes),
                ));
            }
            None if self.scan.is_none() => items.push(note(&t("settings-pane-reading"))),
            None => {}
        }
        let snapshot = self.scan.as_ref().map(|scan| &scan.snapshot);
        let totals = snapshot.map(StorageSnapshot::totals).unwrap_or_default();
        for category in [
            StorageCategory::InstalledApps,
            StorageCategory::AppData,
            StorageCategory::Cache,
            StorageCategory::SyncedFiles,
        ] {
            let value = if category == StorageCategory::SyncedFiles {
                self.sync_snapshot.as_ref().map(|snapshot| {
                    ui::format::size(snapshot.workspaces.iter().map(|w| w.logical_bytes).sum())
                })
            } else {
                snapshot
                    .filter(|snapshot| !snapshot.unavailable.contains(&category))
                    .map(|_| ui::format::size(*totals.get(&category).unwrap_or(&0)))
            };
            if let Some(value) = value {
                items.push(value_row(&category_label(category), &value));
            }
        }
        section(&t("settings-storage-use"), rows(items))
    }

    fn synced_card(&self, actions: &Actions<Settings>) -> SettingsSection {
        if let Some(snapshot) = &self.sync_snapshot {
            let local: u64 = snapshot.workspaces.iter().map(|w| w.local_bytes).sum();
            let remote: u64 = snapshot
                .workspaces
                .iter()
                .map(|w| w.remote_only_bytes)
                .sum();
            let reclaimable: u64 = snapshot
                .workspaces
                .iter()
                .map(|w| w.reclaimable_bytes)
                .sum();
            let mut sync_rows = vec![
                value_row(&t("settings-storage-on-device"), &ui::format::size(local)),
                value_row(
                    &t("settings-storage-remote-only"),
                    &ui::format::size(remote),
                ),
                value_row(
                    &t("settings-storage-safe-to-free"),
                    &ui::format::size(reclaimable),
                ),
            ];
            if let Some(message) = &self.sync_message {
                sync_rows.push(note(message));
            }
            sync_rows.extend(
                snapshot
                    .files
                    .iter()
                    .filter(|f| f.reclaimable_bytes > 0)
                    .take(8)
                    .map(|f| value_row(&f.logical_path, &ui::format::size(f.reclaimable_bytes))),
            );
            if reclaimable > 0 && self.sync_result.is_none() {
                let free = t_args(
                    "settings-storage-free",
                    &[("size", ui::format::size(reclaimable).into())],
                );
                section_with(
                    &t("settings-storage-synced-card"),
                    Box::new(actions.bind_effect_invocation(
                        Button::new("storage.sync.free", &free),
                        &SettingsEffect::from(StorageEffect::FreeSyncedFiles),
                    )),
                    rows(sync_rows),
                )
            } else {
                sync_rows.push(note(&t(if self.sync_result.is_some() {
                    "settings-storage-reclaiming"
                } else if snapshot.online {
                    "settings-storage-nothing-reclaimable"
                } else {
                    "settings-storage-offline"
                })));
                section(&t("settings-storage-synced-card"), rows(sync_rows))
            }
        } else {
            let fallback = t("settings-storage-accounting-unavailable");
            section(
                &t("settings-storage-synced-card"),
                rows(vec![note(
                    self.sync_message.as_deref().unwrap_or(&fallback),
                )]),
            )
        }
    }

    fn apps_card(&self) -> SettingsSection {
        let Some(scan) = &self.scan else {
            return section(
                &t("settings-storage-largest"),
                rows(vec![note(&t("settings-pane-reading"))]),
            );
        };
        let mut largest = scan.snapshot.entries.clone();
        largest.sort_by_key(|e| std::cmp::Reverse(e.bytes));
        let largest_rows = largest
            .into_iter()
            .take(8)
            .map(|e| {
                value_row(
                    &t_args(
                        "settings-storage-item-meta",
                        &[
                            ("owner", e.owner.as_str().into()),
                            ("category", category_label(e.category).into()),
                        ],
                    ),
                    &ui::format::size(e.bytes),
                )
            })
            .collect();
        section(
            &t("settings-storage-largest"),
            if scan.snapshot.entries.is_empty() {
                rows(vec![note(&t("settings-storage-no-apps"))])
            } else {
                rows(largest_rows)
            },
        )
    }

    fn cleanup_card(&self, actions: &Actions<Settings>) -> SettingsSection {
        let Some(scan) = &self.scan else {
            return section(
                &t("settings-storage-cleanup"),
                rows(vec![note(&t("settings-pane-reading"))]),
            );
        };
        match &scan.cleanup {
            Some(preview) if self.cleaning => section(
                &t("settings-storage-cleanup-preview"),
                rows(vec![
                    value_row(
                        &t("settings-storage-cat-cache"),
                        &ui::format::size(preview.total_bytes),
                    ),
                    note(&t("settings-storage-cleaning")),
                ]),
            ),
            Some(preview) if !preview.items.is_empty() => {
                let remove = t_args(
                    "settings-storage-remove",
                    &[("size", ui::format::size(preview.total_bytes).into())],
                );
                let action = Box::new(actions.bind_effect_invocation(
                    Button::new("storage.clean", &remove),
                    &SettingsEffect::from(StorageEffect::CleanUpCaches),
                ));
                let mut preview_rows: Vec<Item> = Vec::new();
                let cache_item = t("settings-storage-cache-item");
                preview_rows.extend(preview.items.iter().take(8).map(|i| {
                    value_row(
                        i.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(cache_item.as_str()),
                        &ui::format::size(i.bytes),
                    )
                }));
                section_with(
                    &t("settings-storage-cleanup-preview"),
                    action,
                    rows(preview_rows),
                )
            }
            _ => section(
                &t("settings-storage-cleanup"),
                rows(vec![note(&t("settings-storage-cleanup-empty"))]),
            ),
        }
    }

    fn cache_card(&self, cx: &ViewCx<Settings>) -> Option<SettingsSection> {
        let prefetch_label = t("settings-storage-prefetch");
        let size_label = t("settings-storage-cache-size");
        let policy = self.cache_policy?;
        let size_row = match cache_size(policy.budget_bytes) {
            Some(size) => cx
                .setting(size_label)
                .id("storage.cache-size")
                .choice_effect(
                    CACHE_SIZES.iter().map(|size| cache_size_shown(size)),
                    &LocalState::new(cache_size_shown(size)),
                    &cx.actions(),
                    effect(StorageEffect::SET_CACHE_SIZE)
                        .map(|shown: String| cache_size_from_label(&shown)),
                ),
            None => value_row(
                &size_label,
                &ui::format::size(policy.budget_bytes.unwrap_or_default()),
            ),
        };
        Some(section(
            &t("settings-storage-cache-card"),
            rows(vec![
                cx.setting(prefetch_label)
                    .described(t("settings-storage-prefetch-desc"))
                    .id("storage.prefetch")
                    .toggle_effect(
                        &LocalState::new(policy.prefetch),
                        &cx.actions(),
                        effect(StorageEffect::SET_PREFETCH),
                    ),
                size_row,
                note(&t("settings-storage-cache-caption")),
            ]),
        ))
    }

    fn tools_card(cx: &ViewCx<Settings>) -> SettingsSection {
        let open_disks = t("settings-storage-open-disks");
        let open_backups = t("settings-storage-open-backups");
        let actions = cx.actions();
        let open = |effect: StorageEffect| {
            let actions = actions.clone();
            move |button| actions.bind_effect_invocation(button, &SettingsEffect::from(effect))
        };
        section(
            &t("settings-storage-tools"),
            rows(vec![
                cx.setting(t("settings-storage-disks"))
                    .described(t("settings-storage-disks-desc"))
                    .id("storage.disks")
                    .button(&open_disks, open(StorageEffect::OpenDisks)),
                cx.setting(t("settings-storage-backups"))
                    .described(t("settings-storage-backups-desc"))
                    .id("storage.backups")
                    .button(&open_backups, open(StorageEffect::OpenBackups)),
                note(&t("settings-storage-tools-caption")),
            ]),
        )
    }
}

impl StorageModel {
    fn busy(&self) -> bool {
        self.sync_result.is_some()
            || self.snapshot_result.is_some()
            || self.scanning.is_some()
            || self.policy_result.is_some()
            || self.residency_job.is_some()
    }
}

struct PolicyAnswer {
    effect: Option<&'static str>,
    receiver: Landing,
}

impl StorageModel {
    fn poll_policy(&mut self) -> bool {
        let Some(answer) = &self.policy_result else {
            return false;
        };
        let result = match answer.receiver.poll::<Result<CachePolicy, String>>() {
            Landed::Done(result) => result,
            Landed::Pending => return false,
            Landed::Lost => Err(t("settings-storage-service-ended")),
        };
        let effect = answer.effect;
        self.policy_result = None;
        match result {
            Ok(policy) => {
                self.cache_policy = Some(policy);
                if let Some(effect) = effect {
                    self.replies
                        .finish(effect, Ok(t("settings-storage-cache-changed")));
                }
            }
            Err(error) => {
                if let Some(effect) = effect {
                    self.replies.finish(
                        effect,
                        Err(t_args(
                            "settings-storage-cache-failed",
                            &[("error", error.into())],
                        )),
                    );
                }
            }
        }
        true
    }

    fn poll_residency_job(&mut self) -> bool {
        let Some((id, receiver)) = &self.residency_job else {
            return false;
        };
        let id = *id;
        let result = match receiver.poll::<Result<String, String>>() {
            Landed::Done(result) => result,
            Landed::Pending => return false,
            Landed::Lost => Err(t("settings-storage-service-ended")),
        };
        self.residency_job = None;
        self.replies.finish(id, result);
        self.refresh_sync();
        true
    }

    fn take_read(&mut self, read: SyncRead) {
        match read.snapshot {
            Ok(snapshot) => {
                self.sync_snapshot = Some(snapshot);
                if self.sync_message.as_deref() == Some(t("settings-storage-loading").as_str()) {
                    self.sync_message = None;
                }
            }
            Err(error) => {
                self.sync_snapshot = None;
                self.sync_message = Some(t_args(
                    "settings-storage-service-unavailable",
                    &[("error", error.into())],
                ));
            }
        }
        match read.residency {
            Some(device) => {
                let summary = device.summary.clone();
                self.fleet.adopt(device);
                if self.summary_waiting {
                    self.answer_summary(&summary);
                }
            }
            None if self.summary_waiting => {
                self.summary_waiting = false;
                self.replies.fail(
                    StorageEffect::STORAGE_SUMMARY.id,
                    t("settings-storage-accounting-unavailable"),
                );
            }
            None => {}
        }
    }
}

fn reclaimable_selections(snapshot: &SyncStorageSnapshot) -> Vec<EvictionSelection> {
    snapshot
        .files
        .iter()
        .filter(|file| file.reclaimable_bytes > 0)
        .map(EvictionSelection::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_over(device: DeviceStorage) -> ui::prelude::TestApp<crate::SettingsShell> {
        let mut app = ui::prelude::TestApp::<crate::SettingsShell>::new().unwrap();
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            let held = area(settings).unwrap();
            held.use_device(std::sync::Arc::new(device));
            let mut model = held.model().borrow_mut();
            for device in crate::scenarios::storage_fixture() {
                model.fleet.adopt(device);
            }
            model.changed = true;
        })
        .expect("the Settings window is mounted");
        crate::effects::show_pane(&mut app, crate::categories::Category::Storage);
        app.settle();
        app
    }

    fn said(app: &mut ui::prelude::TestApp<crate::SettingsShell>) -> Vec<String> {
        provider_settings_ui::widgets::announced_text(app.harness().root_widget())
    }

    #[test]
    fn the_pane_shows_what_only_one_device_holds() {
        let mut app = window_over(DeviceStorage {
            roots: StorageRoots {
                app_payloads: None,
                room_data: None,
                app_cache: None,
                sync_replica: None,
            },
            disk: None,
            reaches_shell: false,
        });
        let text = said(&mut app);
        let says = |needle: &str| text.iter().any(|line| line.contains(needle));
        let at_risk = t_args(
            "settings-storage-only-here-mark",
            &[("size", ui::format::size(48_000_000).into())],
        );
        assert!(
            says(&t_args(
                "settings-storage-this-device",
                &[("device", "Laptop".into())]
            )),
            "{text:#?}"
        );
        assert!(says("Home server") && says("Desktop"), "{text:#?}");
        assert!(says(&at_risk), "{text:#?}");
        assert_eq!(
            text.iter().filter(|line| line.contains(&at_risk)).count(),
            1,
            "only the laptop holds work no other device has: {text:#?}"
        );
        let free = t_args(
            "settings-storage-free-cache",
            &[("size", ui::format::size(1_260_000_000).into())],
        );
        assert!(says(&free), "{text:#?}");
        assert_eq!(
            text.iter()
                .filter(|line| line.contains(&t("settings-storage-keep-here"))
                    || line.contains(&t("settings-storage-stop-keeping")))
                .count(),
            2,
            "one keep control per workspace of this device, none for the others: {text:#?}"
        );
    }

    #[test]
    fn each_workspace_is_listed_once_under_the_disks_real_capacity() {
        let profile = tempfile::tempdir().unwrap();
        let mut app = window_over(DeviceStorage {
            roots: StorageRoots {
                app_payloads: None,
                room_data: None,
                app_cache: None,
                sync_replica: None,
            },
            disk: Some(profile.path().to_path_buf()),
            reaches_shell: false,
        });
        let scanned = |app: &mut ui::prelude::TestApp<crate::SettingsShell>| {
            app.with_widget::<crate::window::Inspected, _>(|settings| {
                area(settings).unwrap().model().borrow().scan.clone()
            })
            .expect("the Settings window is mounted")
        };
        let deadline = tilo_test_env::hang_deadline();
        let scan = loop {
            app.harness()
                .poll_frame()
                .expect("the window draws its next frame");
            if let Some(scan) = scanned(&mut app) {
                break scan;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the scan never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        app.settle();
        // The capacity the scan read from the profile's disk. Read again
        // here, a pooled filesystem (ZFS, a shared overlay) can already
        // report another total: its size moves as other writers use the pool.
        let capacity = scan.capacity.expect("the scan read the disk's capacity");
        let text = said(&mut app);
        let total = ui::format::size(capacity.total_bytes);
        assert!(
            text.iter().any(|line| line.contains(&total)),
            "the disk's own size: {total} in {text:#?}"
        );
        for workspace in ["Thesis", "Family photos"] {
            assert_eq!(
                text.iter()
                    .filter(|line| line.starts_with(workspace))
                    .count(),
                1,
                "{workspace} once: {text:#?}"
            );
        }
        let thesis = text
            .iter()
            .position(|line| line.starts_with("Thesis"))
            .unwrap();
        let kept_on = &text[thesis + 1];
        for keeper in ["Laptop", "Desktop", "Home server"] {
            assert!(kept_on.contains(keeper), "where Thesis is kept: {text:#?}");
        }
        assert!(
            !text.iter().any(|line| line.contains("Not available")),
            "nothing unmeasured is listed: {text:#?}"
        );
    }

    #[test]
    fn the_storage_summary_names_no_file() {
        let summary = &crate::scenarios::storage_fixture()[0].summary;
        let value = serde_json::to_value(summary).unwrap();
        let workspaces = value["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 2);
        for workspace in workspaces {
            let mut keys: Vec<_> = workspace.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["kept_here", "split", "workspace"]);
            assert!(
                workspace["split"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(serde_json::Value::is_u64),
                "only counts: {workspace}"
            );
        }
    }
    #[test]
    fn storage_pane_reports_real_roots_and_reclaims_only_approved_files() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir_all(cache.join("so.tilo.browser")).unwrap();
            std::fs::write(cache.join("so.tilo.browser/http"), vec![0u8; 24]).unwrap();
            let mut pane = StorageModel::over(
                false,
                std::sync::Arc::new(DeviceStorage {
                    roots: StorageRoots {
                        app_payloads: None,
                        room_data: None,
                        app_cache: Some(cache),
                        sync_replica: None,
                    },
                    disk: None,
                    reaches_shell: false,
                }),
            );
            let deadline = tilo_test_env::hang_deadline();
            while pane.scan.is_none() {
                assert!(std::time::Instant::now() < deadline, "the scan lands");
                pane.take_in();
                std::thread::yield_now();
            }
            let scan = pane.scan.as_ref().unwrap();
            assert_eq!(scan.snapshot.reclaimable_bytes(), 24);
            assert_eq!(scan.cleanup.as_ref().unwrap().total_bytes, 24);
        }

        {
            let file = |identity: &str, reclaimable_bytes| runtime_fs::storage::SyncedFileState {
                workspace: "work".into(),
                app: None,
                logical_path: format!("/{identity}"),
                identity: identity.into(),
                inode: 1,
                version: "v".into(),
                logical_bytes: 10,
                hydrated_bytes: 10,
                remote_only: false,
                dirty: reclaimable_bytes == 0,
                pinned: false,
                in_flight: false,
                remotely_durable: reclaimable_bytes > 0,
                reclaimable_bytes,
            };
            let snapshot = SyncStorageSnapshot {
                online: true,
                files: vec![file("clean", 10), file("dirty", 0)],
                ..SyncStorageSnapshot::default()
            };
            let selected = reclaimable_selections(&snapshot);
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].identity, "clean");
        }
    }

    struct StalledStorage(Gate);

    impl StalledStorage {
        fn wait(&self) -> Result<(), String> {
            if self.0.wait() {
                Ok(())
            } else {
                Err("the device never answered".into())
            }
        }
    }

    const STALLED_APP: &str = "so.tilo.stalled-sample";

    impl StorageDevice for StalledStorage {
        fn scan(&self) -> StorageScan {
            crate::device_read::off_ui_thread();
            if self.wait().is_err() {
                return StorageScan::unavailable();
            }
            StorageScan {
                snapshot: StorageSnapshot {
                    entries: vec![desktop_policy::StorageEntry {
                        category: StorageCategory::InstalledApps,
                        owner: STALLED_APP.into(),
                        path: "/nonexistent/stalled".into(),
                        bytes: 42,
                        files: 1,
                        reclaimable: false,
                    }],
                    unavailable: Vec::new(),
                },
                cleanup: None,
                capacity: None,
            }
        }

        fn clean_up(&self, _: &CleanupPreview) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            self.wait()
        }

        fn open_tool(&self, _: ToolIntent, _: Option<[u8; 16]>) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            self.wait()
        }
    }

    type Gate = std::sync::Arc<crate::stall::Stall>;

    fn stalled_window() -> (ui::framework::testing::TestApp<crate::SettingsShell>, Gate) {
        ui::i18n::I18nManager::init("en-US");
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let gate = crate::stall::Stall::new();
        let stalled = std::sync::Arc::clone(&gate);
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            settings
                .areas()
                .find::<crate::panes::storage::StorageArea>()
                .unwrap()
                .use_device(std::sync::Arc::new(StalledStorage(stalled)));
        })
        .expect("the Settings window is mounted");
        (app, gate)
    }

    #[test]
    fn a_stalled_disk_walk_never_holds_the_window() {
        let (mut app, gate) = stalled_window();
        crate::effects::show_pane(&mut app, crate::categories::Category::Storage);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("opening the pane");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("settings-pane-reading")), "{frame}");
        assert!(!frame.contains(STALLED_APP), "{frame}");

        gate.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(STALLED_APP))),
            "what the walk measured is shown"
        );
    }

    #[test]
    fn a_stalled_shell_never_holds_the_window() {
        let (mut app, gate) = stalled_window();
        let invocation = app
            .start_effect(&crate::effects::SettingsEffect::from(
                StorageEffect::OpenDisks,
            ))
            .expect("opening Disks runs as a job");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("the click");
        let ended = |updates: &[ui::tilo_actions::InvocationUpdate]| {
            updates
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        };
        assert!(
            !ended(app.harness().job_updates(invocation)),
            "the job waits for the shell"
        );

        gate.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| ended(
                    harness.job_updates(invocation)
                )),
            "the job ends once the shell answers"
        );
        let opened = app.harness().job_updates(invocation).iter().any(|update| {
            matches!(
                update,
                ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }
                    if output.value == Some(serde_json::Value::String(t("settings-storage-opened-disks")))
            )
        });
        assert!(opened, "the job says Disks opened");
    }

    struct RecordingStorage(ToolsAsked);

    type ToolsAsked = std::sync::Arc<std::sync::Mutex<Vec<(ToolIntent, Option<[u8; 16]>)>>>;

    impl StorageDevice for RecordingStorage {
        fn scan(&self) -> StorageScan {
            StorageScan::unavailable()
        }

        fn clean_up(&self, _: &CleanupPreview) -> Result<(), String> {
            Ok(())
        }

        fn open_tool(
            &self,
            intent: ToolIntent,
            on_behalf_of: Option<[u8; 16]>,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push((intent, on_behalf_of));
            Ok(())
        }
    }

    /// Open Disks pressed by the person asks the shell plainly: their press
    /// in the window is what the shell checks. The same action an agent
    /// invokes through the broker names that invocation, so the shell admits
    /// the open as the agent's.
    #[test]
    fn an_agents_open_disks_names_its_invocation_and_the_persons_does_not() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = std::sync::Arc::clone(&asked);
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            settings
                .areas()
                .find::<crate::panes::storage::StorageArea>()
                .unwrap()
                .use_device(std::sync::Arc::new(RecordingStorage(record)));
        })
        .expect("the Settings window is mounted");
        let open_disks = crate::effects::SettingsEffect::from(StorageEffect::OpenDisks);

        app.effect(&open_disks).expect("the person opens Disks");
        app.effect_by("agent:scribe", &open_disks)
            .expect("the agent opens Disks");

        let asked = std::mem::take(&mut *asked.lock().unwrap());
        let disks = ToolIntent::Disks {
            route: DisksRoute::Overview,
        };
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(
            asked[0],
            (disks.clone(), None),
            "the person's press names nothing"
        );
        assert_eq!(asked[1].0, disks);
        assert!(
            asked[1].1.is_some(),
            "the agent's open names its invocation"
        );
    }

    #[test]
    fn cache_sizes_round_trip_and_stay_above_the_daemons_minimum() {
        assert_eq!(cache_budget("Automatic"), Ok(None));
        assert_eq!(cache_budget("25 GB"), Ok(Some(25_000_000_000)));
        for size in CACHE_SIZES {
            let budget = cache_budget(size).unwrap();
            assert!(
                budget.is_none_or(|bytes| bytes >= runtime_fs::storage::MIN_CACHE_BUDGET_BYTES)
            );
            assert_eq!(cache_size(budget), Some(*size));
            assert_eq!(cache_size_from_label(&cache_size_shown(size)), *size);
            assert_eq!(cache_size_from_label(size), *size);
        }
        assert!(cache_budget("7 GB").is_err());
        assert!(cache_budget("lots").is_err());
        assert_eq!(cache_size(Some(123)), None);
    }
}
