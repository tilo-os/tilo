//! A disk the installer can write to, as one selectable row.
//!
//! The row is the toolkit's `ListRow`, like the drive rows of the image
//! wizard: the recognizable hardware model leads, the capacity sits in its
//! meta column, and the device path with what the disk holds is the
//! secondary line. A disk that cannot be chosen says why on its own row,
//! because a control that is off with no reason reads as a fault. Choosing
//! it, by click, Enter or Space, or assistive activation, fires its
//! [`Control`]: the disk's published `select` action, which the
//! [`crate::setup::destination::installer_app::InstallerApp`] runs through
//! its action door.

use ui::icons::Icon;
use ui::widget::ListRow;

use crate::setup::destination::control::{Bound, Control};
use imager_backend::destination::disks::Disk;

/// The capacity a person reads for `disk`: its size in the locale's binary
/// units, or `lsblk`'s own text when discovery could not read the byte count.
pub fn capacity(disk: &Disk) -> String {
    if disk.size_bytes > 0 {
        ui::format::memory(disk.size_bytes)
    } else {
        disk.size.trim().to_string()
    }
}

/// Whether the installer refuses to write to `disk`.
fn unavailable(disk: &Disk) -> bool {
    disk.blocks_install() || disk.read_only || !disk.identity_ready()
}

/// Why `disk` cannot be chosen, or what choosing it shares with the running
/// installer: each a sentence of its own.
fn notes(disk: &Disk) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if disk.hosts_staged_installer() {
        notes.push(
            "This disk also holds the running installer and your existing system. Tilo is added only in the space prepared for it.",
        );
    } else if disk.is_system {
        notes.push("Unavailable because this is the disk the live system is running from.");
    }
    if disk.read_only {
        notes.push("Unavailable because this disk is read-only.");
    }
    if !disk.identity_ready() {
        notes.push(
            "Unavailable because the installer could not verify which physical disk this is. Refresh the disk list or reconnect it.",
        );
    }
    notes
}

/// The row for `disk`, choosing it through `control`; `selected` when it is
/// the disk the wizard holds. Serial numbers, WWNs and kernel device
/// numbers bind the backend's safety check; they are never shown or spoken.
pub fn disk_row(disk: &Disk, control: &Control, selected: bool) -> Bound {
    let model = disk.model.trim();
    let title = if model.is_empty() {
        format!("{} storage device", capacity(disk))
    } else {
        model.to_string()
    };
    let notes = notes(disk);
    let mut secondary = format!("{}, {}", disk.name.trim(), disk.contents_summary());
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
        .on_effect(move || fire.fire());
    Bound::new(Box::new(row), [(control.effect().console_id(), control)]).activated_by_keys(control)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use automerge::{AutoCommit, ROOT, ReadDoc};
    use taffy::TaffyTree;
    use ui::synthetic_input::named_key;
    use ui::theme::Theme;
    use ui::widget::Widget;
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

            assert!(row.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
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
            let bindings = ui::widget::controls::control_action_bindings(&row);
            assert_eq!(bindings.len(), 1, "the row is listed as its select action");
            assert!(matches!(
                bindings[0].kind,
                ui::widget::ControlBindingKind::Action(_)
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
            assert_eq!(node.name.as_deref(), Some("QEMU Fast Storage"));
            let value = node.value.as_deref().unwrap_or_default();
            assert!(value.contains("40 GiB"), "{value}");
            assert!(value.contains("/dev/vdb, Empty disk"), "{value}");
            assert_eq!(node.state.checked, Some(true));
            assert!(!node.state.disabled);
            assert!(!value.contains("secret-wwn") && !value.contains("252:16"));
        }
        {
            let node = row(&disk(), false)
                .accessibility()
                .expect("disk selection semantics");
            assert_eq!(node.name.as_deref(), Some("40 GiB storage device"));
            assert_eq!(node.state.checked, Some(false));
        }
        // Phase `an_unavailable_disk_says_every_reason_and_runs_nothing`.
        {
            let mut target = disk();
            target.is_system = true;
            target.read_only = true;
            target.major_minor.clear();
            let (control, presses) = select();
            let mut row = disk_row(&target, &control, false);

            let node = row.accessibility().expect("disabled disk semantics");
            let value = node.value.as_deref().expect("disabled reason");
            assert!(node.state.disabled);
            assert!(!row.is_focusable());
            assert!(value.contains("live system is running from"));
            assert!(value.contains("disk is read-only"));
            assert!(value.contains("could not verify which physical disk"));

            let mut doc = AutoCommit::new();
            assert!(!row.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
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
            let value = node.value.as_deref().expect("staged description");
            assert!(value.contains("also holds the running installer"));
            assert!(!value.contains("Unavailable"));
        }
    }
}
