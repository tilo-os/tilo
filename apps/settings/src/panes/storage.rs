//! Storage overview. It accounts bytes here; partitioning and backup workflows
//! remain in their specialist tools, opened with typed contextual intents.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use desktop_policy::{CleanupPreview, StorageCategory, StorageRoots, StorageSnapshot};
use os_actions::{BackupsRoute, DisksRoute, ToolIntent};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{caption, card, card_with_action, rows, value_row};

const CLEAN_KEY: &str = "storage.cleanup.confirmed";
const DISKS_KEY: &str = "storage.open.disks";
const BACKUPS_KEY: &str = "storage.open.backups";

pub struct StoragePane {
    roots: StorageRoots,
    snapshot: StorageSnapshot,
    cleanup: Option<CleanupPreview>,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl StoragePane {
    pub fn new() -> Self {
        let roots = StorageRoots::discover();
        Self::with_roots(roots)
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
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let totals = self.snapshot.totals();
        let overview = card(
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
                    let value = if self.snapshot.unavailable.contains(&category) {
                        "Not available".into()
                    } else {
                        human_bytes(*totals.get(&category).unwrap_or(&0))
                    };
                    value_row(category.label(), &value)
                })
                .collect(),
            ),
        );

        let mut largest = self.snapshot.entries.clone();
        largest.sort_by(|a, b| b.bytes.cmp(&a.bytes));
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
        let apps = card(
            "Largest items",
            if self.snapshot.entries.is_empty() {
                rows(vec![caption(
                    "No qOS application storage was found on this device.",
                )])
            } else {
                rows(largest_rows)
            },
        );

        let cleanup = match &self.cleanup {
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
        };

        let tools = card(
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
        );
        crate::widgets::cards(vec![overview, apps, cleanup, tools])
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

impl Widget for StoragePane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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
}
