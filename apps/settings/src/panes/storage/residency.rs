//! What each of the person's devices holds of their workspaces, by what
//! losing that device would cost: kept by choice, also held by another
//! device, held only there, and cache that is safe to free.
//!
//! A device's figures are its storage summary, the answer of its
//! `storage-summary` action: counts per workspace, never a file name, so
//! nothing of a workspace's contents is listed by it. This device's comes
//! from its own storage service.

use runtime_fs::storage::WorkspaceResidency;
use ui::widget::{ChipStyle, RowMenu};

use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::widgets::{SettingsSection, StatusTone, note, row, rows, section, thing_row};

use super::StorageEffect;

pub use crate::scenarios::DeviceResidency;

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

/// One list of the person's workspaces, each once: what this device holds
/// of it and what losing this device would cost, and which of the person's
/// devices keep it. This device's rows free its cache and keep a workspace
/// here.
pub fn page(cx: &PaneCx, fleet: &Fleet, busy: bool) -> Vec<SettingsSection> {
    if fleet.devices().is_empty() {
        return Vec::new();
    }
    let mut names: Vec<&str> = Vec::new();
    for device in fleet.devices() {
        for workspace in &device.summary.workspaces {
            if !names.contains(&workspace.workspace.as_str()) {
                names.push(&workspace.workspace);
            }
        }
    }
    let mut items: Vec<_> = names
        .into_iter()
        .map(|name| workspace_row(cx, fleet, name, busy))
        .collect();
    if items.is_empty() {
        items.push(note(&t("settings-storage-no-workspaces")));
    }
    vec![section(&t("settings-storage-workspaces"), rows(items))]
}

/// A device's name as the workspace rows list it.
fn device_name(device: &DeviceResidency) -> String {
    if device.current && device.label.is_empty() {
        t("settings-storage-this-device-unnamed")
    } else if device.current {
        t_args(
            "settings-storage-this-device",
            &[("device", device.label.as_str().into())],
        )
    } else {
        device.label.clone()
    }
}

fn workspace_row(cx: &PaneCx, fleet: &Fleet, name: &str, busy: bool) -> crate::widgets::Item {
    fn held_by<'a>(device: &'a DeviceResidency, name: &str) -> Option<&'a WorkspaceResidency> {
        device
            .summary
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace == name)
    }
    let here = fleet.this_device().and_then(|device| held_by(device, name));
    let keepers: Vec<String> = fleet
        .devices()
        .iter()
        .filter(|device| held_by(device, name).is_some())
        .map(device_name)
        .collect();
    let size = |bytes| ui::format::size(bytes);
    let mut tags: Vec<(String, StatusTone)> = Vec::new();
    let mut details = vec![t_args(
        "settings-storage-kept-on",
        &[("devices", keepers.join(", ").into())],
    )];
    if let Some(workspace) = here {
        let split = &workspace.split;
        if split.only_here_bytes > 0 {
            tags.push((
                t_args(
                    "settings-storage-only-here-mark",
                    &[("size", size(split.only_here_bytes).into())],
                ),
                StatusTone::Warning,
            ));
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
        if workspace.kept_here {
            tags.push((t("settings-storage-kept-here-mark"), StatusTone::Neutral));
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
    } else {
        details.push(t("settings-storage-nothing-here"));
    }
    let menu = here.filter(|_| !busy).map(|workspace| {
        let split = &workspace.split;
        let mut menu = RowMenu::new(
            format!("storage.residency.more.{name}"),
            &t_args("settings-row-more", &[("name", name.to_owned().into())]),
        );
        if split.cache_bytes > 0 {
            menu = menu.action(
                t_args(
                    "settings-storage-free-cache",
                    &[("size", size(split.cache_bytes).into())],
                ),
                cx.invocation_binding(StorageEffect::FreeCache {
                    workspace: name.to_owned(),
                }),
            );
        }
        let (key, keep) = if workspace.kept_here {
            ("settings-storage-stop-keeping", false)
        } else {
            ("settings-storage-keep-here", true)
        };
        Box::new(menu.action(
            t(key),
            cx.invocation_binding(StorageEffect::KeepHere {
                workspace: name.to_owned(),
                keep,
            }),
        )) as Box<dyn ui::widget::Widget>
    });
    let tags: Vec<(&str, ChipStyle)> = tags
        .iter()
        .map(|(text, tone)| (text.as_str(), ChipStyle::Tinted(*tone)))
        .collect();
    row(Box::new(
        thing_row(
            &format!("storage.residency.workspace.{name}"),
            name,
            &details,
            menu,
        )
        .icon(ui::icons::Icon::Layers)
        .tags(&tags),
    ))
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
            summary: runtime_fs::storage::ResidencySummary {
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
