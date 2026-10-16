//! Slots follow the OS image they were made on.
//!
//! A layer is built against one image: its libraries, its `Cargo.lock`, the
//! protocol versions its other components speak. Once the machine boots
//! another image (a bootc upgrade, switch or rollback), a layer from before
//! would run an old program against new ones; that is how an upgraded
//! laptop kept running its old shell, launcher and tilo-vmd and broke on a
//! protocol mismatch. So the first thing a boot does, before
//! `systemd-sysext` merges anything, is compare the booted image with the
//! one recorded in `<root>/image.json`. When they differ every slot goes
//! back to the image itself (generation 0), the generations made for the
//! old image are deleted (nothing may activate them again), tilo-vmd's
//! links in `/var/lib/extensions` go, and `<root>/image-change.json` says
//! what was dropped, for `tilo-vm status` and the upgrade's own report.
//! The layers nobody references any more are deleted later in the boot
//! ([`collect_dropped_layers`]), where the time it takes delays no unit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::{DroppedLayers, ImageChange};

use crate::apply::{self, Apply};
use crate::error::DaemonError;
use crate::fsutil::write_atomic;
use crate::layers::LayerStore;
use crate::slots::{SlotStore, now};

/// The image the slots' generations were made on.
const IMAGE_FILE: &str = "image.json";
/// The last [`ImageChange`].
const CHANGE_FILE: &str = "image-change.json";
/// Present while the layers of a change are still to be deleted.
const COLLECT_MARKER: &str = "collect-layers";

pub use tilo_vm_protocol::booted_image;

/// The booted image of this machine; see [`booted_image`].
#[must_use]
pub fn running_image() -> Option<String> {
    booted_image(&std::fs::read_to_string("/proc/cmdline").ok()?)
}

#[derive(Debug, Serialize, Deserialize)]
struct Recorded {
    image: String,
    recorded_at: String,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DaemonError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
    write_atomic(path, &bytes)
}

/// The image the slots' generations are made on, as last recorded.
#[must_use]
pub fn slots_image(root: &Path) -> Option<String> {
    read_json::<Recorded>(&root.join(IMAGE_FILE)).map(|r| r.image)
}

/// The last change [`reconcile`] made, if any.
#[must_use]
pub fn last_change(root: &Path) -> Option<ImageChange> {
    read_json(&root.join(CHANGE_FILE))
}

/// Where the reconcile runs, which decides how the dropped links leave
/// `/usr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// Before `systemd-sysext.service`: nothing is merged yet, so removing
    /// the links is enough.
    Pending,
    /// Later in the boot, with the layers already merged: `/usr` is
    /// refreshed without them.
    Done,
}

/// Put every slot back on `booted` when its generations were made on
/// another image; see the module documentation. `None` when nothing
/// changed.
///
/// A state root without `image.json` was written by a tilo-vmd that did not
/// record images. Every boot of an image carrying this code records it, so
/// such state is from an earlier image and its layers are dropped too.
///
/// # Errors
/// I/O failures. A slot that cannot be reset stops the reconcile before the
/// image is recorded, so the next boot tries again.
pub fn reconcile(
    root: &Path,
    slots: &SlotStore,
    apply: &mut dyn Apply,
    booted: &str,
    merge: Merge,
) -> Result<Option<ImageChange>, DaemonError> {
    let recorded: Option<Recorded> = read_json(&root.join(IMAGE_FILE));
    if recorded.as_ref().is_some_and(|r| r.image == booted) {
        return Ok(None);
    }
    let mut dropped = Vec::new();
    let mut sessions = Vec::new();
    for mut record in slots.list()? {
        let generation = slots.generation(&record.name, record.current).ok();
        let ran_layers =
            record.current != 0 && generation.as_ref().is_none_or(|g| !g.layers.is_empty());
        if ran_layers {
            dropped.push(DroppedLayers {
                slot: record.name.clone(),
                generation: record.current,
                components: generation
                    .map(|g| g.components.into_keys().collect())
                    .unwrap_or_default(),
            });
            if let Some(session) = record.session().filter(|s| s.login.is_none()) {
                sessions.push(session);
            }
        }
        if record.current != 0 || !slots.generations(&record.name).is_empty() {
            slots.return_to_image(&mut record)?;
        }
    }
    // tilo-vmd's links only; anything another tool put there stays.
    apply.link_sysext(&[])?;
    if merge == Merge::Done && !dropped.is_empty() {
        apply.sysext_refresh()?;
    }
    // A slot session reads its layers from drop-ins; each is rewritten empty
    // before anything starts the session. The owner's greetd login has none:
    // its layers came through the links above.
    let mut rewrote = false;
    for session in &sessions {
        let Ok(uid) = apply.account_uid(&session.user) else {
            // No account yet: the session has never run, and the restore
            // writes its drop-ins before it starts it.
            continue;
        };
        let text = apply::layers_dropin(&session.slot, 0, &[]);
        for unit in apply::layered_units(session, uid) {
            apply.write_dropin(&unit, apply::LAYERS_DROPIN, &text)?;
            rewrote = true;
        }
    }
    if rewrote {
        apply.reload()?;
    }
    let change = ImageChange {
        from: recorded.map(|r| r.image),
        to: booted.to_owned(),
        at: now(),
        dropped,
        freed_bytes: 0,
    };
    if !change.dropped.is_empty() {
        write_json(&root.join(CHANGE_FILE), &change)?;
        write_atomic(&root.join(COLLECT_MARKER), b"")?;
    }
    write_json(
        &root.join(IMAGE_FILE),
        &Recorded {
            image: booted.to_owned(),
            recorded_at: change.at.clone(),
        },
    )?;
    Ok(Some(change))
}

/// Delete the layers a change left unreferenced: every layer no remaining
/// generation of any slot names and older than `min_age` (a younger one may
/// be an import whose swap is still to come, and waits for a later boot).
/// Returns the bytes freed; 0 when no change is waiting.
///
/// # Errors
/// I/O failures; the marker stays, so the next boot tries again.
pub fn collect_dropped_layers(
    root: &Path,
    slots: &SlotStore,
    layers: &LayerStore,
    min_age: std::time::Duration,
) -> Result<u64, DaemonError> {
    let marker = root.join(COLLECT_MARKER);
    if !marker.exists() {
        return Ok(0);
    }
    let mut referenced = BTreeSet::new();
    for record in slots.list()? {
        for number in slots.generations(&record.name) {
            referenced.extend(slots.generation(&record.name, number)?.layers);
        }
    }
    let collected = layers.collect(&referenced, min_age, false)?;
    if let Some(mut change) = last_change(root) {
        change.freed_bytes = change.freed_bytes.saturating_add(collected.freed);
        write_json(&root.join(CHANGE_FILE), &change)?;
    }
    // A layer too young to delete is looked at again at the next boot.
    if collected.young.is_empty() {
        std::fs::remove_file(&marker)
            .map_err(DaemonError::io("clear the layer collection marker"))?;
    }
    Ok(collected.freed)
}

/// The state root, created as tmpfiles would (0700) when this runs before
/// `systemd-tmpfiles-setup` on a machine's first boot.
///
/// # Errors
/// I/O failures.
pub fn ensure_root(root: &Path) -> Result<PathBuf, DaemonError> {
    use std::os::unix::fs::DirBuilderExt;
    if !root.is_dir() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(DaemonError::io(format!("create {}", root.display())))?;
    }
    Ok(root.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tilo_vm_protocol::{ComponentInfo, Owner, Scope, SlotKind};

    use super::*;
    use crate::apply::RecordingApply;

    #[test]
    fn the_booted_image_is_read_from_the_kernel_command_line() {
        assert_eq!(
            booted_image("root=x rw composefs=91e8abc quiet").as_deref(),
            Some("composefs:91e8abc")
        );
        assert_eq!(
            booted_image("ostree=/ostree/boot.1/default/abc/0 rw").as_deref(),
            Some("ostree:/ostree/boot.1/default/abc/0")
        );
        assert_eq!(booted_image("root=/dev/vda2 composefs= rw"), None);
        assert_eq!(booted_image("root=/dev/vda2 rw"), None);
    }

    fn session_info() -> ComponentInfo {
        ComponentInfo::new(Scope::Session, vec![])
    }

    fn vm_info() -> ComponentInfo {
        ComponentInfo::new(Scope::Vm, vec!["greetd.service".into()])
    }

    fn import(layers: &LayerStore, name: &str) -> String {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("usr/bin")).unwrap();
        std::fs::write(source.path().join("usr/bin").join(name), name).unwrap();
        layers
            .import_path(
                source.path(),
                &tilo_vm_protocol::Provenance::local(name),
                &mut |_| Ok(()),
            )
            .unwrap()
            .0
    }

    fn swap(slots: &SlotStore, name: &str, component: &str, layer: &str, info: ComponentInfo) {
        let mut record = slots.load(name).unwrap();
        let (_, to) = slots
            .prepare_swap(&record, component, layer, info)
            .unwrap()
            .unwrap();
        slots.set_current(&mut record, to.number).unwrap();
    }

    /// The upgraded laptop: the `vm` slot ran swapped greeter and tilo-vmd
    /// builds, a headless slot a swapped shell. After booting another image
    /// both are back on the image, the old generations are gone, the
    /// session's drop-ins name no layer, nothing links into the layer store,
    /// the change says what was dropped, and the layers are deleted later.
    #[test]
    fn booting_another_image_puts_every_slot_back_on_it() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        slots
            .create("a", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        let shell = import(&layers, "shell");
        let greeter = import(&layers, "greeter");
        swap(&slots, "a", "shell", &shell, session_info());
        swap(&slots, "vm", "greeter", &greeter, vm_info());
        let mut apply = RecordingApply::default();

        // First boot with this code on an image that already ran slots: no
        // record, so the layers are from an earlier image.
        let change = reconcile(
            root.path(),
            &slots,
            &mut apply,
            "composefs:new",
            Merge::Pending,
        )
        .unwrap()
        .expect("a change");
        assert_eq!(change.from, None);
        assert_eq!(change.to, "composefs:new");
        let dropped: Vec<(&str, u64, Vec<String>)> = change
            .dropped
            .iter()
            .map(|d| (d.slot.as_str(), d.generation, d.components.clone()))
            .collect();
        assert_eq!(
            dropped,
            vec![
                ("a", 1, vec!["shell".to_owned()]),
                ("vm", 1, vec!["greeter".to_owned()])
            ]
        );
        for name in ["a", "vm"] {
            assert_eq!(slots.load(name).unwrap().current, 0, "{name}");
            assert!(slots.generations(name).is_empty(), "{name}");
        }
        assert_eq!(last_change(root.path()), Some(change.clone()));
        assert_eq!(slots_image(root.path()).as_deref(), Some("composefs:new"));
        let text = apply.calls.join("\n");
        assert!(text.contains("link_sysext 0"), "{text}");
        // Nothing is merged yet before systemd-sysext, so nothing refreshes.
        assert!(!text.contains("sysext_refresh"), "{text}");
        assert!(
            text.contains("write_dropin tilo-vm-session@a.service 50-tilo-vm-layers.conf"),
            "{text}"
        );
        assert!(change.describe()[1].contains("slot a left generation 1 (shell)"));

        // The same image again: nothing to do.
        assert_eq!(
            reconcile(
                root.path(),
                &slots,
                &mut apply,
                "composefs:new",
                Merge::Done
            )
            .unwrap(),
            None
        );

        // Later in the boot the dropped layers go.
        let freed = collect_dropped_layers(root.path(), &slots, &layers, Duration::ZERO).unwrap();
        assert!(freed > 0);
        assert!(layers.list().unwrap().is_empty());
        assert_eq!(last_change(root.path()).unwrap().freed_bytes, freed);
        assert_eq!(
            collect_dropped_layers(root.path(), &slots, &layers, Duration::ZERO).unwrap(),
            0,
            "collected once"
        );
    }

    /// Swaps made on the booted image survive its reboots; the next image
    /// change drops them, and with the layers already merged `/usr` is
    /// refreshed without them.
    #[test]
    fn swaps_on_the_booted_image_survive_its_reboots() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let mut apply = RecordingApply::default();
        assert_eq!(
            reconcile(
                root.path(),
                &slots,
                &mut apply,
                "composefs:one",
                Merge::Pending
            )
            .unwrap()
            .map(|c| c.dropped),
            Some(Vec::new()),
            "a fresh machine records its image and drops nothing"
        );
        assert!(last_change(root.path()).is_none());
        let greeter = import(&layers, "greeter");
        swap(&slots, "vm", "greeter", &greeter, vm_info());
        assert_eq!(
            reconcile(
                root.path(),
                &slots,
                &mut apply,
                "composefs:one",
                Merge::Pending
            )
            .unwrap(),
            None
        );
        assert_eq!(slots.load("vm").unwrap().current, 1);

        apply.calls.clear();
        let change = reconcile(
            root.path(),
            &slots,
            &mut apply,
            "composefs:two",
            Merge::Done,
        )
        .unwrap()
        .unwrap();
        assert_eq!(change.from.as_deref(), Some("composefs:one"));
        assert_eq!(change.dropped.len(), 1);
        assert!(
            apply.calls.iter().any(|l| l == "sysext_refresh"),
            "{:?}",
            apply.calls
        );
        // A layer imported within the minimum age is kept.
        assert_eq!(
            collect_dropped_layers(root.path(), &slots, &layers, Duration::from_secs(3600))
                .unwrap(),
            0
        );
        assert_eq!(layers.list().unwrap().len(), 1);
    }
}
