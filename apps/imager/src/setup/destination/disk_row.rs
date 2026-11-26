//! A disk the installer can write to, as one selectable row.

use tilo_ui_base::icons::Icon;
use ui::widget::ListRow;

use crate::setup::destination::control::{Bound, Control};
use imager_backend::destination::disks::Disk;

pub fn capacity(disk: &Disk) -> String {
    if disk.size_bytes > 0 {
        tilo_ui_base::format::memory(disk.size_bytes)
    } else {
        disk.size.trim().to_string()
    }
}

/// Why Tilo cannot go on a disk at all: it is smaller than the room Tilo's partitions take.
pub fn too_small() -> String {
    crate::setup::strings::t_args(
        "installer-blocked-disk-too-small",
        &[(
            "minimum",
            tilo_ui_base::i18n::ArgValue::Text(tilo_ui_base::format::memory(imager_backend::MIN_TARGET_BYTES)),
        )],
    )
}

fn unavailable(disk: &Disk) -> bool {
    disk.blocks_install() || disk.read_only || disk.too_small() || !disk.identity_ready()
}

fn notes(disk: &Disk) -> Vec<String> {
    let mut notes = Vec::new();
    if disk.hosts_staged_installer() {
        notes.push(
            "This disk also holds the running installer and your existing system. Tilo is added only in the space prepared for it.".to_owned(),
        );
    } else if disk.is_system {
        notes.push(
            "Unavailable because this is the disk the live system is running from.".to_owned(),
        );
    }
    if disk.read_only {
        notes.push("Unavailable because this disk is read-only.".to_owned());
    }
    if disk.too_small() {
        notes.push(too_small());
    }
    if !disk.identity_ready() {
        notes.push(
            "Unavailable because the installer could not verify which physical disk this is. Refresh the disk list or reconnect it.".to_owned(),
        );
    }
    notes
}

pub fn disk_row(disk: &Disk, control: &Control, selected: bool) -> Bound {
    let model = disk.model.trim();
    let title = if model.is_empty() {
        crate::resource::CATALOG.t_args(
            "imager-storage-device",
            &[("size", tilo_ui_base::i18n::ArgValue::Text(capacity(disk)))],
        )
    } else {
        model.to_string()
    };
    let notes = notes(disk);
    let mut secondary = crate::contents::drive_line(
        disk.name.trim(),
        disk.partitions.len(),
        &disk.partition_filesystems(),
    );
    for note in &notes {
        secondary.push_str(". ");
        secondary.push_str(note.trim_end_matches('.'));
    }
    let fire = control.clone();
    let row = ListRow::new(disk.name.as_str(), title)
        .icon(if disk.removable {
            Icon::Usb
        } else {
            Icon::HardDrive
        })
        .secondary(secondary)
        .secondary_lines(if notes.is_empty() { 1 } else { 3 })
        .meta(capacity(disk))
        .selected(selected)
        .checked(selected)
        .with_enabled(!unavailable(disk))
        .on_activate(move || fire.fire());
    Bound::new(Box::new(row), [(control.effect().console_id(), control)]).activated_by_keys(control)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use automerge::{AutoCommit, ROOT, ReadDoc};
    use taffy::TaffyTree;
    use tilo_ui_base::synthetic_input::named_key;
    use tilo_ui_base::theme::Theme;
    use tilo_ui_base::widget::Widget;
    use winit::keyboard::NamedKey;

    use super::*;
    use crate::setup::destination::control::Presses;
    use crate::setup::destination::effects::DestinationEffect;

    fn disk() -> Disk {
        Disk {
            name: "/dev/vdb".into(),
            size: "40.0 GiB".into(),
            model: String::new(),
            size_bytes: 40 * 1024 * 1024 * 1024,
            partition_table: "gpt".into(),
            partition_table_uuid: "11111111-1111-1111-1111-111111111111".into(),
            logical_sector_size: 512,
            major_minor: "252:16".into(),
            wwn: String::new(),
            serial: "tilo-test-target".into(),
            removable: false,
            read_only: false,
            is_system: false,
            live_system_nodes: Vec::new(),
            internal_plan: None,
            partitions: Vec::new(),
        }
    }

    fn select() -> (Control, Presses) {
        Control::detached(DestinationEffect::SelectDisk {
            device: "/dev/vdb".into(),
        })
    }

    fn row(target: &Disk, selected: bool) -> Bound {
        disk_row(target, &select().0, selected)
    }

    fn assert_key_selects(key: NamedKey) {
        let (control, presses) = select();
        let mut row = disk_row(&disk(), &control, false);
        row.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = row.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node)
            .send(&mut row, &named_key(key));
        assert!(
            presses.take().map(|press| press.effect)
                == Some(DestinationEffect::SelectDisk {
                    device: "/dev/vdb".into()
                })
        );
        assert_eq!(doc.length(ROOT), 0);
    }

    #[test]
    fn disk_row_selection_is_one_action_for_keys_and_assistive_activation() {
        {
            assert_key_selects(NamedKey::Enter);
            assert_key_selects(NamedKey::Space);
        }
        {
            let mut target = disk();
            target.major_minor.clear();
            assert!(!row(&target, false).is_focusable());
        }
        {
            let (control, presses) = select();
            let mut row = disk_row(&disk(), &control, false);
            let mut doc = AutoCommit::new();

            crate::setup::destination::press(&mut row, &tilo_ui_base::a11y::Action::Activate, &mut doc);
            assert!(
                presses.take().map(|press| press.effect)
                    == Some(DestinationEffect::SelectDisk {
                        device: "/dev/vdb".into()
                    })
            );
            assert_eq!(doc.length(ROOT), 0);
        }
        {
            let row = row(&disk(), false);
            let bindings = tilo_ui_base::widget::controls::control_action_bindings(&row);
            assert_eq!(bindings.len(), 1, "the row is listed as its select action");
            assert!(matches!(
                bindings[0].kind,
                tilo_ui_base::widget::ControlBindingKind::Action(_)
            ));
            assert_eq!(row.test_id(), Some("/dev/vdb"));
        }
    }

    #[test]
    fn disk_row_names_the_disk_and_every_barrier() {
        {
            let mut target = disk();
            target.model = "QEMU Fast Storage".into();
            target.wwn = "secret-wwn".into();
            let node = row(&target, true)
                .accessibility()
                .expect("disk selection semantics");
            assert_eq!(
                node.name.as_deref(),
                Some("QEMU Fast Storage, /dev/vdb, Empty disk")
            );
            let value = node.value.as_deref().unwrap_or_default();
            assert!(value.contains("40 GiB"), "{value}");
            assert_eq!(node.state.checked, Some(true));
            assert!(!node.state.disabled);
            assert!(!value.contains("secret-wwn") && !value.contains("252:16"));
        }
        {
            let node = row(&disk(), false)
                .accessibility()
                .expect("disk selection semantics");
            assert_eq!(
                node.name.as_deref(),
                Some("40 GiB storage device, /dev/vdb, Empty disk")
            );
            assert_eq!(node.state.checked, Some(false));
        }
        {
            let mut target = disk();
            target.is_system = true;
            target.read_only = true;
            target.major_minor.clear();
            let (control, presses) = select();
            let mut row = disk_row(&target, &control, false);

            let node = row.accessibility().expect("disabled disk semantics");
            let name = node.name.as_deref().expect("disabled reason");
            assert!(node.state.disabled);
            assert!(!row.is_focusable());
            assert!(name.contains("live system is running from"));
            assert!(name.contains("disk is read-only"));
            assert!(name.contains("could not verify which physical disk"));

            let mut doc = AutoCommit::new();
            assert!(!row.perform_action(
                &tilo_ui_base::a11y::Action::Activate,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
        }
        {
            let mut target = disk();
            target.size_bytes = 20 << 30;
            let row = row(&target, false);
            let node = row.accessibility().expect("too small disk semantics");
            assert!(node.state.disabled);
            assert!(!row.is_focusable());
            let name = node.name.as_deref().expect("too small reason");
            assert!(
                name.contains("too small: Tilo needs at least 35 GiB"),
                "{name}"
            );
        }
        {
            let mut target = disk();
            target.is_system = true;
            target.internal_plan = Some(imager_backend::destination::disks::GuidedAlongsidePlan {
                free_start_bytes: 1 << 30,
                free_size_bytes: 35 << 30,
                efi_start_bytes: 1 << 30,
                efi_size_bytes: 2 << 30,
                boot_start_bytes: 3 << 30,
                boot_size_bytes: 1 << 30,
                root_start_bytes: 4 << 30,
                root_size_bytes: 32 << 30,
            });
            let row = row(&target, false);
            let node = row.accessibility().expect("staged disk semantics");
            assert!(!node.state.disabled);
            assert!(row.is_focusable());
            let name = node.name.as_deref().expect("staged description");
            assert!(name.contains("also holds the running installer"));
            assert!(!name.contains("Unavailable"));
        }
    }
}
