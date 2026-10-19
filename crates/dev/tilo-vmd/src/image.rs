//! Which OS image the slots' generations run on.
//!
//! A layer is built against one image: its libraries, its `Cargo.lock`, the
//! protocol versions its other components speak. Once the machine boots
//! another image (a bootc upgrade, switch or rollback), a layer from before
//! may run an old program against new ones. The layers are the person's
//! deployed work all the same, so a boot never takes them off a slot and
//! never deletes them: every boot records the booted image in
//! `<root>/image.json`, and when it differs from the one recorded at the
//! boot before, `<root>/image-change.json` names the slots that still run
//! layers built on the previous image, for `tilo-vm status` and the
//! upgrade's own report. The person then rolls each slot back
//! (`tilo-vm rollback --slot <slot> --to 0` runs the image as installed) or
//! forward (a new swap builds against the booted image).
//!
//! An unknown previous image is never a change: a state root without
//! `image.json` was written by a tilo-vmd that did not record images (on
//! 2026-10-19 such a root counted as one, and the first boot of a recording
//! tilo-vmd dropped every slot's layers and deleted 7.8 GB of them), so it
//! only gets the booted image recorded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::{ImageChange, SlotLayers};

use crate::error::DaemonError;
use crate::fsutil::write_atomic;
use crate::slots::{SlotStore, now};

/// The image the last boot ran.
const IMAGE_FILE: &str = "image.json";
/// The last [`ImageChange`].
const CHANGE_FILE: &str = "image-change.json";
/// Left by an older tilo-vmd for the layers it dropped, which it deleted
/// later in the boot. Nothing deletes layers at boot any more; the marker
/// is removed so no reader takes it for pending work.
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

/// The image recorded at the last boot.
#[must_use]
pub fn slots_image(root: &Path) -> Option<String> {
    read_json::<Recorded>(&root.join(IMAGE_FILE)).map(|r| r.image)
}

/// The last change [`reconcile`] found, if any.
#[must_use]
pub fn last_change(root: &Path) -> Option<ImageChange> {
    read_json(&root.join(CHANGE_FILE))
}

/// Record `booted` as the image the slots run on; see the module
/// documentation. Returns the change when the boot before recorded another
/// image, `None` when it recorded this one or none at all. No slot, no
/// generation, no link and no layer is touched either way.
///
/// # Errors
/// I/O failures; the image stays unrecorded, so the next boot tries again.
pub fn reconcile(
    root: &Path,
    slots: &SlotStore,
    booted: &str,
) -> Result<Option<ImageChange>, DaemonError> {
    let recorded: Option<Recorded> = read_json(&root.join(IMAGE_FILE));
    if recorded.as_ref().is_some_and(|r| r.image == booted) {
        return Ok(None);
    }
    let at = now();
    let change = match recorded {
        // Nothing says which image the slots were made on: not a change.
        None => None,
        Some(previous) => {
            let mut kept = Vec::new();
            for record in slots.list()? {
                if record.current == 0 {
                    continue;
                }
                let generation = slots.generation(&record.name, record.current).ok();
                if generation.as_ref().is_some_and(|g| g.layers.is_empty()) {
                    continue;
                }
                kept.push(SlotLayers {
                    slot: record.name.clone(),
                    generation: record.current,
                    components: generation
                        .map(|g| g.components.into_keys().collect())
                        .unwrap_or_default(),
                });
            }
            let change = ImageChange {
                from: Some(previous.image),
                to: booted.to_owned(),
                at: at.clone(),
                kept,
                dropped: Vec::new(),
                freed_bytes: 0,
            };
            write_json(&root.join(CHANGE_FILE), &change)?;
            Some(change)
        }
    };
    match std::fs::remove_file(root.join(COLLECT_MARKER)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(DaemonError::io("remove the old layer collection marker")(e));
        }
        _ => {}
    }
    write_json(
        &root.join(IMAGE_FILE),
        &Recorded {
            image: booted.to_owned(),
            recorded_at: at,
        },
    )?;
    Ok(change)
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
    use tilo_vm_protocol::{ComponentInfo, Owner, Scope, SlotKind};

    use super::*;
    use crate::layers::LayerStore;

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

    /// Every slot's generation and every layer, as the boot sees them.
    fn state(slots: &SlotStore, layers: &LayerStore) -> (Vec<(String, u64, Vec<u64>)>, usize) {
        let mut seen: Vec<(String, u64, Vec<u64>)> = slots
            .list()
            .unwrap()
            .into_iter()
            .map(|r| (r.name.clone(), r.current, slots.generations(&r.name)))
            .collect();
        seen.sort();
        (seen, layers.list().unwrap().len())
    }

    /// The laptop on 2026-10-19: the `vm` slot ran a swapped tilo-vmd that
    /// records images, swapped in while the machine ran one that did not,
    /// so no image was recorded. Its first boot found no `image.json`,
    /// counted that as an image change, put every slot (the device's
    /// generation 106 among them) back on the image and deleted 7.8 GB of
    /// layers: the person came up on the bare image. An unknown previous
    /// image is no change: the image is recorded and nothing else moves.
    #[test]
    fn an_unknown_previous_image_is_no_change_and_drops_nothing() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        slots
            .create("a", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        let shell = import(&layers, "shell");
        let vmd = import(&layers, "vmd");
        swap(&slots, "a", "shell", &shell, session_info());
        swap(&slots, "vm", "vmd", &vmd, vm_info());
        // The marker an older tilo-vmd left to delete layers later.
        std::fs::write(root.path().join(COLLECT_MARKER), b"").unwrap();
        let before = state(&slots, &layers);

        let change = reconcile(root.path(), &slots, "composefs:1c9ab8").unwrap();

        assert_eq!(change, None, "no recorded image, so no change");
        assert_eq!(last_change(root.path()), None);
        assert_eq!(
            slots_image(root.path()).as_deref(),
            Some("composefs:1c9ab8")
        );
        assert_eq!(state(&slots, &layers), before, "slots and layers untouched");
        assert!(!root.path().join(COLLECT_MARKER).exists());
    }

    /// Layers are the person's deployed work: booting another image keeps
    /// every slot on its generation with every layer in place, and says
    /// which slots run layers built on the previous image so the person
    /// can roll them back or forward deliberately. The same image again
    /// changes nothing and keeps the report.
    #[test]
    fn booting_another_image_keeps_every_slot_and_layer_and_says_so() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        assert_eq!(
            reconcile(root.path(), &slots, "composefs:one").unwrap(),
            None,
            "a fresh machine records its image"
        );
        slots
            .create("a", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        slots
            .create("idle", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        let shell = import(&layers, "shell");
        let greeter = import(&layers, "greeter");
        swap(&slots, "a", "shell", &shell, session_info());
        swap(&slots, "vm", "greeter", &greeter, vm_info());
        assert_eq!(
            reconcile(root.path(), &slots, "composefs:one").unwrap(),
            None
        );
        let before = state(&slots, &layers);

        let change = reconcile(root.path(), &slots, "composefs:two")
            .unwrap()
            .expect("a change");

        assert_eq!(change.from.as_deref(), Some("composefs:one"));
        assert_eq!(change.to, "composefs:two");
        let kept: Vec<(&str, u64, Vec<String>)> = change
            .kept
            .iter()
            .map(|d| (d.slot.as_str(), d.generation, d.components.clone()))
            .collect();
        assert_eq!(
            kept,
            vec![
                ("a", 1, vec!["shell".to_owned()]),
                ("vm", 1, vec!["greeter".to_owned()])
            ]
        );
        assert!(change.dropped.is_empty());
        assert_eq!(state(&slots, &layers), before, "slots and layers untouched");
        assert_eq!(last_change(root.path()), Some(change.clone()));
        assert_eq!(slots_image(root.path()).as_deref(), Some("composefs:two"));
        let said = change.describe().join("\n");
        assert!(
            said.contains("slot a stays on generation 1 (shell)"),
            "{said}"
        );
        assert!(
            said.contains("tilo-vm rollback --slot <slot> --to 0"),
            "{said}"
        );
        assert!(!said.contains("dropped"), "{said}");

        assert_eq!(
            reconcile(root.path(), &slots, "composefs:two").unwrap(),
            None
        );
        assert_eq!(last_change(root.path()), Some(change));
        assert_eq!(state(&slots, &layers), before);
    }

    /// The record the laptop holds from the older tilo-vmd still reads as
    /// what happened.
    #[test]
    fn a_drop_an_older_tilo_vmd_recorded_still_reads_as_one() {
        let record = r#"{"from": null, "to": "composefs:1c9ab8", "at": "2026-10-19T19:32:21Z",
            "dropped": [{"slot": "device", "generation": 106, "components": ["shell"]}],
            "freed_bytes": 7834890240}"#;
        let change: ImageChange = serde_json::from_str(record).unwrap();
        assert!(change.kept.is_empty());
        let said = change.describe().join("\n");
        assert!(said.contains("an older tilo-vmd dropped"), "{said}");
        assert!(
            said.contains("slot device left generation 106 (shell)"),
            "{said}"
        );
    }
}
