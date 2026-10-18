//! What each of the person's devices holds of their workspaces, by what
//! losing that device would cost: kept by choice, also held by another
//! device, held only there, and cache that is safe to free.
//!
//! A device's figures are its storage summary, the answer of its
//! `storage-summary` action: counts per workspace, never a file name, so
//! nothing of a workspace's contents is listed by it. This device's comes
//! from its own storage service.

use runtime_fs::storage::{ResidencySummary, WorkspaceResidency};
use ui::widget::Button;

use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::widgets::{SettingsSection, StatusTone, card, item_row, note, rows, status_pill};

use super::StorageEffect;

/// One device's storage summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceResidency {
    /// The device's key, in hex; what its summaries are kept by.
    pub key: String,
    /// The name the person gave the device.
    pub label: String,
    /// Whether it is the device Settings runs on.
    pub current: bool,
    pub summary: ResidencySummary,
}

/// The summaries known, one per device: this device first, then the others
/// by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fleet {
    devices: Vec<DeviceResidency>,
}

impl Fleet {
    /// Take in `device`'s summary, in place of any it gave before.
    pub fn adopt(&mut self, device: DeviceResidency) {
        match self
            .devices
            .iter_mut()
            .find(|known| known.key == device.key)
        {
            Some(known) => *known = device,
            None => self.devices.push(device),
        }
        self.devices.sort_by(|a, b| {
            b.current
                .cmp(&a.current)
                .then_with(|| a.label.cmp(&b.label))
        });
    }

    /// Drop the cards of other devices `listed` (their keys) no longer
    /// names; this device's stays.
    pub fn keep_listed(&mut self, listed: &[String]) {
        self.devices
            .retain(|device| device.current || listed.contains(&device.key));
    }

    pub fn devices(&self) -> &[DeviceResidency] {
        &self.devices
    }

    /// This device's summary, once read.
    pub fn this_device(&self) -> Option<&DeviceResidency> {
        self.devices.iter().find(|device| device.current)
    }
}

/// One card per device: each workspace it holds, and what of it would be
/// lost with the device. This device's rows free its cache and keep a
/// workspace here.
pub fn cards(cx: &PaneCx, fleet: &Fleet, busy: bool) -> Vec<SettingsSection> {
    fleet
        .devices()
        .iter()
        .map(|device| {
            let title = if device.current && device.label.is_empty() {
                t("settings-storage-this-device-unnamed")
            } else if device.current {
                t_args(
                    "settings-storage-this-device",
                    &[("device", device.label.as_str().into())],
                )
            } else {
                device.label.clone()
            };
            let mut items: Vec<_> = device
                .summary
                .workspaces
                .iter()
                .map(|workspace| workspace_row(cx, device.current && !busy, workspace))
                .collect();
            if items.is_empty() {
                items.push(note(&t("settings-storage-no-workspaces")));
            }
            card(&title, rows(items))
        })
        .collect()
}

fn workspace_row(
    cx: &PaneCx,
    actions: bool,
    workspace: &WorkspaceResidency,
) -> crate::widgets::Item {
    let split = &workspace.split;
    let size = |bytes| ui::format::size(bytes);
    let mut marks = Vec::new();
    if split.only_here_bytes > 0 {
        marks.push(status_pill(
            &t_args(
                "settings-storage-only-here-mark",
                &[("size", size(split.only_here_bytes).into())],
            ),
            StatusTone::Warning,
        ));
    }
    if workspace.kept_here {
        marks.push(status_pill(
            &t("settings-storage-kept-here-mark"),
            StatusTone::Neutral,
        ));
    }
    let mut details = Vec::new();
    if split.only_here_bytes > 0 {
        details.push(t_args(
            "settings-storage-only-here",
            &[
                ("size", size(split.only_here_bytes).into()),
                (
                    "files",
                    ui::i18n::ArgValue::Number(split.only_here_files as f64),
                ),
            ],
        ));
    }
    for (key, bytes) in [
        ("settings-storage-replicated", split.replicated_bytes),
        ("settings-storage-pinned", split.pinned_bytes),
        ("settings-storage-cache", split.cache_bytes),
    ] {
        if bytes > 0 {
            details.push(t_args(key, &[("size", size(bytes).into())]));
        }
    }
    if details.is_empty() {
        details.push(t("settings-storage-nothing-here"));
    }
    let mut buttons = Vec::new();
    if actions {
        if split.cache_bytes > 0 {
            let label = t_args(
                "settings-storage-free-cache",
                &[("size", size(split.cache_bytes).into())],
            );
            buttons.push(cx.bind_invocation(
                Button::new(
                    &format!("storage.residency.free.{}", workspace.workspace),
                    &label,
                ),
                StorageEffect::FreeCache {
                    workspace: workspace.workspace.clone(),
                },
            ));
        }
        let (key, keep) = if workspace.kept_here {
            ("settings-storage-stop-keeping", false)
        } else {
            ("settings-storage-keep-here", true)
        };
        buttons.push(cx.bind_invocation(
            Button::new(
                &format!("storage.residency.keep.{}", workspace.workspace),
                &t(key),
            ),
            StorageEffect::KeepHere {
                workspace: workspace.workspace.clone(),
                keep,
            },
        ));
    }
    item_row(&workspace.workspace, marks, &details, buttons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_fs::storage::ResidencySplit;

    fn device(key: &str, label: &str, current: bool, only_here: u64) -> DeviceResidency {
        DeviceResidency {
            key: key.into(),
            label: label.into(),
            current,
            summary: ResidencySummary {
                workspaces: vec![WorkspaceResidency {
                    workspace: "notes".into(),
                    kept_here: false,
                    split: ResidencySplit {
                        only_here_bytes: only_here,
                        ..ResidencySplit::default()
                    },
                }],
            },
        }
    }

    /// Summaries are kept by device: a newer one replaces the device's
    /// last, and this device comes first.
    #[test]
    fn summaries_are_kept_one_per_device_with_this_one_first() {
        let mut fleet = Fleet::default();
        fleet.adopt(device("b", "Server", false, 1));
        fleet.adopt(device("c", "Laptop", false, 2));
        fleet.adopt(device("a", "Desk", true, 3));
        fleet.adopt(device("b", "Server", false, 4));
        let order: Vec<_> = fleet.devices().iter().map(|d| d.key.as_str()).collect();
        assert_eq!(order, ["a", "c", "b"]);
        assert_eq!(fleet.this_device().map(|d| d.key.as_str()), Some("a"));
        assert_eq!(
            fleet.devices()[2].summary.workspaces[0]
                .split
                .only_here_bytes,
            4,
            "the newer summary replaced the older"
        );
        fleet.keep_listed(&["c".to_owned()]);
        let kept: Vec<_> = fleet.devices().iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            kept,
            ["a", "c"],
            "a device the list no longer names loses its card; this one keeps its"
        );
    }
}
