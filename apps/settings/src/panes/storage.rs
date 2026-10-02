//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.

use std::collections::HashMap;
use ui::doc::read_bool;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, transaction::Transactable};
use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use runtime_fs::storage::{
    DEFAULT_STORAGE_SOCKET, EvictionReport, EvictionSelection, StorageClient, SyncStorageSnapshot,
};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::i18n::ArgValue;

use crate::i18n::{t, t_args};
use crate::widgets::{El, caption, card, card_with_action, rows, value_row};

fn category_label(category: StorageCategory) -> String {
    t(match category {
        StorageCategory::InstalledApps => "settings-storage-cat-apps",
        StorageCategory::AppData => "settings-storage-cat-data",
        StorageCategory::Cache => "settings-storage-cat-cache",
        StorageCategory::SyncedFiles => "settings-storage-cat-synced",
    })
}

// These are edge-triggered, machine-local command latches, not settings.
// Buttons may mutate them directly because the `.edit` namespace is never
// exported or replayed; durable settings go through SettingsAction instead.
pub const CLEAN_KEY: &str = "storage.cleanup.confirmed.edit";
pub const DISKS_KEY: &str = "storage.open.disks.edit";
pub const BACKUPS_KEY: &str = "storage.open.backups.edit";
pub const SYNC_FREE_KEY: &str = "storage.sync.free.edit";

pub struct StoragePane {
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
    sync_snapshot: Option<SyncStorageSnapshot>,
    sync_message: Option<String>,
    sync_busy: bool,
    sync_result: Option<std::sync::mpsc::Receiver<Result<EvictionReport, String>>>,
    snapshot_result: Option<std::sync::mpsc::Receiver<Result<SyncStorageSnapshot, String>>>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl StoragePane {
    pub fn new() -> Self {
        let roots = StorageRoots::discover();
        let mut pane = Self::with_roots(roots);
        pane.refresh_sync();
        pane
    }

    pub fn with_roots(roots: StorageRoots) -> Self {
        let snapshot = desktop_policy::scan_storage(&roots);
        let cleanup = roots
            .app_cache
            .as_deref()
            .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok());
        Self {
            roots,
            snapshot,
            cleanup,
            sync_snapshot: None,
            sync_message: None,
            sync_busy: false,
            sync_result: None,
            snapshot_result: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn refresh_sync(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                .and_then(|mut client| client.snapshot())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        if self.sync_snapshot.is_none() {
            self.sync_message = Some(t("settings-storage-loading"));
        }
        self.snapshot_result = Some(receiver);
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers construct the exact typed request whose token is completed"
    )]
    fn finish_local_operation(
        &self,
        operation: crate::actions::StorageOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::STORAGE_OPERATION_TYPE,
            &operation,
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        crate::widgets::cards(vec![
            self.overview_card(),
            self.synced_card(),
            self.apps_card(),
            self.cleanup_card(),
            Self::tools_card(),
        ])
    }

    fn overview_card(&self) -> El {
        let totals = self.snapshot.totals();
        card(
            &t("settings-storage-use"),
            rows(
                [
                    StorageCategory::InstalledApps,
                    StorageCategory::AppData,
                    StorageCategory::Cache,
                    StorageCategory::SyncedFiles,
                ]
                .into_iter()
                .map(|category| {
                    let value = if category == StorageCategory::SyncedFiles {
                        self.sync_snapshot.as_ref().map_or_else(
                            || t("settings-storage-unavailable"),
                            |snapshot| {
                                ui::format::bytes_si(
                                    snapshot.workspaces.iter().map(|w| w.logical_bytes).sum(),
                                )
                            },
                        )
                    } else if self.snapshot.unavailable.contains(&category) {
                        t("settings-storage-unavailable")
                    } else {
                        ui::format::bytes_si(*totals.get(&category).unwrap_or(&0))
                    };
                    value_row(&category_label(category), &value)
                })
                .collect(),
            ),
        )
    }

    fn synced_card(&self) -> El {
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
                value_row(
                    &t("settings-storage-on-device"),
                    &ui::format::bytes_si(local),
                ),
                value_row(
                    &t("settings-storage-remote-only"),
                    &ui::format::bytes_si(remote),
                ),
                value_row(
                    &t("settings-storage-safe-to-free"),
                    &ui::format::bytes_si(reclaimable),
                ),
            ];
            if let Some(message) = &self.sync_message {
                sync_rows.push(caption(message));
            }
            sync_rows.extend(
                snapshot
                    .files
                    .iter()
                    .filter(|f| f.reclaimable_bytes > 0)
                    .take(8)
                    .map(|f| {
                        value_row(&f.logical_path, &ui::format::bytes_si(f.reclaimable_bytes))
                    }),
            );
            if reclaimable > 0 && !self.sync_busy {
                let free = t_args(
                    "settings-storage-free",
                    &[("size", ui::format::bytes_si(reclaimable).into())],
                );
                card_with_action(
                    &t("settings-storage-synced-card"),
                    Box::new(
                        Button::new("storage.sync.free", &free, Some(SYNC_FREE_KEY))
                            .content_sized(),
                    ),
                    rows(sync_rows),
                )
            } else {
                sync_rows.push(caption(&t(if self.sync_busy {
                    "settings-storage-reclaiming"
                } else if snapshot.online {
                    "settings-storage-nothing-reclaimable"
                } else {
                    "settings-storage-offline"
                })));
                card(&t("settings-storage-synced-card"), rows(sync_rows))
            }
        } else {
            let fallback = t("settings-storage-accounting-unavailable");
            card(
                &t("settings-storage-synced-card"),
                rows(vec![caption(
                    self.sync_message.as_deref().unwrap_or(&fallback),
                )]),
            )
        }
    }

    fn apps_card(&self) -> El {
        let mut largest = self.snapshot.entries.clone();
        largest.sort_by_key(|e| std::cmp::Reverse(e.bytes));
        let largest_rows = largest
            .into_iter()
            .take(8)
            .map(|e| {
                value_row(
                    &e.owner,
                    &t_args(
                        "settings-storage-item-meta",
                        &[
                            ("size", ui::format::bytes_si(e.bytes).into()),
                            ("category", category_label(e.category).into()),
                        ],
                    ),
                )
            })
            .collect();
        card(
            &t("settings-storage-largest"),
            if self.snapshot.entries.is_empty() {
                rows(vec![caption(&t("settings-storage-no-apps"))])
            } else {
                rows(largest_rows)
            },
        )
    }

    fn cleanup_card(&self) -> El {
        match &self.cleanup {
            Some(preview) if !preview.items.is_empty() => {
                let remove = t_args(
                    "settings-storage-remove",
                    &[("size", ui::format::bytes_si(preview.total_bytes).into())],
                );
                let action = Box::new(
                    Button::new("storage.clean", &remove, Some(CLEAN_KEY)).content_sized(),
                );
                let mut preview_rows = vec![caption(&t("settings-storage-cleanup-caption"))];
                let cache_item = t("settings-storage-cache-item");
                preview_rows.extend(preview.items.iter().take(8).map(|i| {
                    value_row(
                        i.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(cache_item.as_str()),
                        &ui::format::bytes_si(i.bytes),
                    )
                }));
                card_with_action(
                    &t("settings-storage-cleanup-preview"),
                    action,
                    rows(preview_rows),
                )
            }
            _ => card(
                &t("settings-storage-cleanup"),
                rows(vec![caption(&t("settings-storage-cleanup-empty"))]),
            ),
        }
    }

    fn tools_card() -> El {
        let open_disks = t("settings-storage-open-disks");
        let open_backups = t("settings-storage-open-backups");
        card(
            &t("settings-storage-tools"),
            rows(vec![
                crate::widgets::setting_row_described(
                    &t("settings-storage-disks"),
                    &t("settings-storage-disks-desc"),
                    Box::new(
                        Button::new("storage.disks", &open_disks, Some(DISKS_KEY)).content_sized(),
                    ),
                ),
                crate::widgets::setting_row_described(
                    &t("settings-storage-backups"),
                    &t("settings-storage-backups-desc"),
                    Box::new(
                        Button::new("storage.backups", &open_backups, Some(BACKUPS_KEY))
                            .content_sized(),
                    ),
                ),
                caption(&t("settings-storage-tools-caption")),
            ]),
        )
    }
}

impl Default for StoragePane {
    fn default() -> Self {
        Self::new()
    }
}
pub fn build() -> Box<dyn Widget> {
    Box::new(StoragePane::new())
}

fn reclaimable_selections(snapshot: &SyncStorageSnapshot) -> Vec<EvictionSelection> {
    snapshot
        .files
        .iter()
        .filter(|file| file.reclaimable_bytes > 0)
        .map(EvictionSelection::from)
        .collect()
}

impl Widget for StoragePane {
    #[allow(
        clippy::too_many_lines,
        reason = "one pass deliberately coordinates the storage snapshot and each independent command latch"
    )]
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        if let Some(crate::actions::LocalPaneCommand::Storage(operation)) = self.command_state.get()
        {
            self.command_state.set(None);
            let key = match operation {
                crate::actions::StorageOperation::CleanupCaches => CLEAN_KEY,
                crate::actions::StorageOperation::FreeSyncedFiles => SYNC_FREE_KEY,
                crate::actions::StorageOperation::OpenDisks => DISKS_KEY,
                crate::actions::StorageOperation::OpenBackups => BACKUPS_KEY,
            };
            let _ = state.put(&root, key, true);
        }
        {
            let state = &mut state;
            let root = &root;
            if let Some(receiver) = &self.snapshot_result {
                match receiver.try_recv() {
                    Ok(Ok(snapshot)) => {
                        self.sync_snapshot = Some(snapshot);
                        self.snapshot_result = None;
                        if self.sync_message.as_deref()
                            == Some(t("settings-storage-loading").as_str())
                        {
                            self.sync_message = None;
                        }
                    }
                    Ok(Err(error)) => {
                        self.sync_snapshot = None;
                        self.snapshot_result = None;
                        self.sync_message = Some(t_args(
                            "settings-storage-service-unavailable",
                            &[("error", error.into())],
                        ));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.sync_snapshot = None;
                        self.snapshot_result = None;
                        self.sync_message = Some(t("settings-storage-service-ended"));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            if let Some(receiver) = &self.sync_result {
                match receiver.try_recv() {
                    Ok(Ok(report)) => {
                        self.sync_busy = false;
                        self.sync_result = None;
                        self.refresh_sync();
                        self.sync_message =
                            Some(if report.queued_files > 0 && report.evicted_files > 0 {
                                t_args(
                                    "settings-storage-freed-queued",
                                    &[
                                        ("size", ui::format::bytes_si(report.freed_bytes).into()),
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
                                        ("size", ui::format::bytes_si(report.freed_bytes).into()),
                                        ("evicted", ArgValue::Number(report.evicted_files as f64)),
                                    ],
                                )
                            });
                        self.finish_local_operation(
                            crate::actions::StorageOperation::FreeSyncedFiles,
                            Ok(t("settings-storage-reclaim-done")),
                        );
                    }
                    Ok(Err(error)) => {
                        self.sync_busy = false;
                        self.sync_result = None;
                        let message = t_args(
                            "settings-storage-nothing-removed",
                            &[("error", error.into())],
                        );
                        self.sync_message = Some(message.clone());
                        self.finish_local_operation(
                            crate::actions::StorageOperation::FreeSyncedFiles,
                            Err(message),
                        );
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.sync_busy = false;
                        self.sync_result = None;
                        self.sync_message = Some(t("settings-storage-request-ended"));
                        self.finish_local_operation(
                            crate::actions::StorageOperation::FreeSyncedFiles,
                            Err(t("settings-storage-service-ended")),
                        );
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            if read_bool(state, root, CLEAN_KEY).unwrap_or(false) {
                let _ = state.put(root, CLEAN_KEY, false);
                if let Some(preview) = &self.cleanup {
                    let selected: Vec<usize> = (0..preview.items.len()).collect();
                    match desktop_policy::apply_cleanup(preview, &selected) {
                        Ok(_) => {
                            self.snapshot = desktop_policy::scan_storage(&self.roots);
                            self.cleanup = self
                                .roots
                                .app_cache
                                .as_deref()
                                .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok());
                            self.finish_local_operation(
                                crate::actions::StorageOperation::CleanupCaches,
                                Ok(t("settings-storage-cleaned")),
                            );
                        }
                        Err(error) => self.finish_local_operation(
                            crate::actions::StorageOperation::CleanupCaches,
                            Err(t_args(
                                "settings-storage-clean-failed",
                                &[("error", error.to_string().into())],
                            )),
                        ),
                    }
                } else {
                    self.finish_local_operation(
                        crate::actions::StorageOperation::CleanupCaches,
                        Err(t("settings-storage-no-cache")),
                    );
                }
            }
            if read_bool(state, root, SYNC_FREE_KEY).unwrap_or(false) {
                let _ = state.put(root, SYNC_FREE_KEY, false);
                if !self.sync_busy
                    && let Some(snapshot) = &self.sync_snapshot
                {
                    let selected = reclaimable_selections(snapshot);
                    let (sender, receiver) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let result = StorageClient::connect(DEFAULT_STORAGE_SOCKET)
                            .and_then(|mut client| client.evict(selected))
                            .map_err(|error| error.to_string());
                        let _ = sender.send(result);
                    });
                    self.sync_busy = true;
                    self.sync_result = Some(receiver);
                } else {
                    self.finish_local_operation(
                        crate::actions::StorageOperation::FreeSyncedFiles,
                        Err(t("settings-storage-no-reclaimable")),
                    );
                }
            }
            if read_bool(state, root, DISKS_KEY).unwrap_or(false) {
                let _ = state.put(root, DISKS_KEY, false);
                let result = os_actions::open_tool(ToolIntent::Disks {
                    route: DisksRoute::Overview,
                });
                self.finish_local_operation(
                    crate::actions::StorageOperation::OpenDisks,
                    result.map(|()| t("settings-storage-opened-disks")),
                );
            }
            if read_bool(state, root, BACKUPS_KEY).unwrap_or(false) {
                let _ = state.put(root, BACKUPS_KEY, false);
                let result = os_actions::open_tool(ToolIntent::Backups {
                    route: BackupsRoute::Overview,
                });
                self.finish_local_operation(
                    crate::actions::StorageOperation::OpenBackups,
                    result.map(|()| t("settings-storage-opened-backups")),
                );
            }
            if let Some(c) = &mut self.composed {
                c.sync(state, root);
            }
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        // `child` was just created in this same tree, so this call is not
        // reachable to fail; if it (impossibly) did, wrap it in itself
        // rather than panic this panic=abort process.
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn name(&self) -> &'static str {
        "StoragePane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Only real roots are reported and only authority-approved files are reclaimed.
    #[test]
    fn storage_pane_reports_real_roots_and_reclaims_only_approved_files() {
        // Case `pane_reports_only_real_roots`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir_all(cache.join("so.tilo.browser")).unwrap();
            std::fs::write(cache.join("so.tilo.browser/http"), vec![0u8; 24]).unwrap();
            let pane = StoragePane::with_roots(StorageRoots {
                app_payloads: None,
                room_data: None,
                app_cache: Some(cache),
                sync_replica: None,
            });
            assert_eq!(pane.snapshot.reclaimable_bytes(), 24);
            assert_eq!(pane.cleanup.as_ref().unwrap().total_bytes, 24);
        }

        // Case `settings_submits_only_authority_approved_reclaimable_files`.
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
}
