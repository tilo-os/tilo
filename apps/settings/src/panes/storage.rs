//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
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
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{El, caption, card, card_with_action, rows, value_row};

const CLEAN_KEY: &str = "storage.cleanup.confirmed";
const DISKS_KEY: &str = "storage.open.disks";
const BACKUPS_KEY: &str = "storage.open.backups";
const SYNC_FREE_KEY: &str = "storage.sync.free";

pub struct StoragePane {
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
    sync_snapshot: Option<SyncStorageSnapshot>,
    sync_message: Option<String>,
    sync_busy: bool,
    sync_result: Option<std::sync::mpsc::Receiver<Result<EvictionReport, String>>>,
    snapshot_result: Option<std::sync::mpsc::Receiver<Result<SyncStorageSnapshot, String>>>,
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
            self.sync_message = Some("Loading synced storage…".into());
        }
        self.snapshot_result = Some(receiver);
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
            "Storage use",
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
                            || "Not available".into(),
                            |snapshot| {
                                human_bytes(
                                    snapshot.workspaces.iter().map(|w| w.logical_bytes).sum(),
                                )
                            },
                        )
                    } else if self.snapshot.unavailable.contains(&category) {
                        "Not available".into()
                    } else {
                        human_bytes(*totals.get(&category).unwrap_or(&0))
                    };
                    value_row(category.label(), &value)
                })
                .collect(),
            ),
        )
    }

    fn synced_card(&self) -> El {
        match &self.sync_snapshot {
            Some(snapshot) => {
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
                    value_row("On this device", &human_bytes(local)),
                    value_row("On other devices only", &human_bytes(remote)),
                    value_row("Safe to free", &human_bytes(reclaimable)),
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
                        .map(|f| value_row(&f.logical_path, &human_bytes(f.reclaimable_bytes))),
                );
                if reclaimable > 0 && !self.sync_busy {
                    card_with_action(
                        "Synced storage",
                        Box::new(Button::new(
                            "storage.sync.free",
                            &format!("Free {}", human_bytes(reclaimable)),
                            Some(SYNC_FREE_KEY),
                        )),
                        rows(sync_rows),
                    )
                } else {
                    sync_rows.push(caption(if self.sync_busy {
                        "Verifying remote copies and freeing local space…"
                    } else if snapshot.online {
                        "Nothing is safely reclaimable. Dirty, pinned, shared, and unverified content stays local."
                    } else {
                        "You are offline. Local synced content stays available and cannot be reclaimed."
                    }));
                    card("Synced storage", rows(sync_rows))
                }
            }
            None => card(
                "Synced storage",
                rows(vec![caption(
                    self.sync_message
                        .as_deref()
                        .unwrap_or("Sync storage accounting is not available."),
                )]),
            ),
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
                    &format!("{} · {}", human_bytes(e.bytes), e.category.label()),
                )
            })
            .collect();
        card(
            "Largest items",
            if self.snapshot.entries.is_empty() {
                rows(vec![caption(
                    "No qOS application storage was found on this device.",
                )])
            } else {
                rows(largest_rows)
            },
        )
    }

    fn cleanup_card(&self) -> El {
        match &self.cleanup {
            Some(preview) if !preview.items.is_empty() => {
                let action = Box::new(Button::new(
                    "storage.clean",
                    &format!("Remove {}", human_bytes(preview.total_bytes)),
                    Some(CLEAN_KEY),
                ));
                let mut preview_rows = vec![caption(
                    "Only the cache items listed below will be removed. App data, synced files, and installed packages are never part of this action.",
                )];
                preview_rows.extend(preview.items.iter().take(8).map(|i| {
                    value_row(
                        i.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Cache item"),
                        &human_bytes(i.bytes),
                    )
                }));
                card_with_action("Cache cleanup preview", action, rows(preview_rows))
            }
            _ => card(
                "Cache cleanup",
                rows(vec![caption("There is no application cache to clean.")]),
            ),
        }
    }

    fn tools_card() -> El {
        card(
            "Storage tools",
            rows(vec![
                crate::widgets::setting_row_described(
                    "Disks",
                    "Partitions, formatting, drive health, and removable media",
                    Box::new(Button::new("storage.disks", "Open Disks", Some(DISKS_KEY))),
                ),
                crate::widgets::setting_row_described(
                    "Backups",
                    "Backup history, schedules, retention, and restore",
                    Box::new(Button::new(
                        "storage.backups",
                        "Open Backups",
                        Some(BACKUPS_KEY),
                    )),
                ),
                caption(
                    "These open the exact specialist workspace through the shell. Settings does not duplicate destructive disk or restore workflows.",
                ),
            ]),
        )
    }

    fn pressed(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
        matches!(state.get(root, key), Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true)))
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

fn human_bytes(bytes: u64) -> String {
    for (unit, scale) in [("GB", 1_000_000_000), ("MB", 1_000_000), ("kB", 1_000)] {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(receiver) = &self.snapshot_result {
            match receiver.try_recv() {
                Ok(Ok(snapshot)) => {
                    self.sync_snapshot = Some(snapshot);
                    self.snapshot_result = None;
                    if self.sync_message.as_deref() == Some("Loading synced storage…") {
                        self.sync_message = None;
                    }
                }
                Ok(Err(error)) => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some(format!("Sync storage service unavailable: {error}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_snapshot = None;
                    self.snapshot_result = None;
                    self.sync_message = Some("Sync storage service ended unexpectedly.".into());
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
                    self.sync_message = Some(
                        if report.queued_files > 0 && report.evicted_files > 0 {
                            format!(
                                "Freed {} from {} synced file(s); {} more queued for verified background reclamation.",
                                human_bytes(report.freed_bytes),
                                report.evicted_files,
                                report.queued_files
                            )
                        } else if report.queued_files > 0 {
                            format!(
                                "{} synced file(s) queued for verified background reclamation; nothing is counted as free until GC confirms it.",
                                report.queued_files
                            )
                        } else {
                            format!(
                                "Freed {} from {} synced files.",
                                human_bytes(report.freed_bytes),
                                report.evicted_files
                            )
                        },
                    );
                }
                Ok(Err(error)) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    self.sync_message = Some(format!("Nothing was removed: {error}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync_busy = false;
                    self.sync_result = None;
                    self.sync_message = Some(
                        "The sync storage request ended unexpectedly; nothing was assumed removed."
                            .into(),
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if Self::pressed(state, root, CLEAN_KEY) {
            let _ = state.put(root, CLEAN_KEY, false);
            if let Some(preview) = &self.cleanup {
                let selected: Vec<usize> = (0..preview.items.len()).collect();
                if desktop_policy::apply_cleanup(preview, &selected).is_ok() {
                    self.snapshot = desktop_policy::scan_storage(&self.roots);
                    self.cleanup = self
                        .roots
                        .app_cache
                        .as_deref()
                        .and_then(|p| desktop_policy::preview_cache_cleanup(p).ok());
                }
            }
        }
        if Self::pressed(state, root, SYNC_FREE_KEY) {
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
            }
        }
        if Self::pressed(state, root, DISKS_KEY) {
            let _ = state.put(root, DISKS_KEY, false);
            let _ = os_actions::open_tool(ToolIntent::Disks {
                route: DisksRoute::Overview,
            });
        }
        if Self::pressed(state, root, BACKUPS_KEY) {
            let _ = state.put(root, BACKUPS_KEY, false);
            let _ = os_actions::open_tool(ToolIntent::Backups {
                route: BackupsRoute::Overview,
            });
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
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
    #[test]
    fn pane_reports_only_real_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(cache.join("dev.qos.browser")).unwrap();
        std::fs::write(cache.join("dev.qos.browser/http"), vec![0u8; 24]).unwrap();
        let pane = StoragePane::with_roots(StorageRoots {
            app_payloads: None,
            room_data: None,
            app_cache: Some(cache),
            sync_replica: None,
        });
        assert_eq!(pane.snapshot.reclaimable_bytes(), 24);
        assert_eq!(pane.cleanup.as_ref().unwrap().total_bytes, 24);
    }

    #[test]
    fn settings_submits_only_authority_approved_reclaimable_files() {
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
