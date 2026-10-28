//! Which OS image the slots' generations run on, and which of their layers
//! fit it.
//!
//! A layer is built against one image: its libraries, its `Cargo.lock`, the
//! interface versions its other components speak. Once the machine boots
//! another image (a bootc upgrade, switch or rollback), a layer from before
//! may run an old program against new ones. The layers are the person's
//! deployed work all the same: a boot never takes them off a slot and never
//! deletes them (only an explicit `tilo-vm rollback` or `gc` deletes). Every
//! boot records the booted image in `<root>/image.json`; when it differs
//! from the one recorded at the boot before, each layer any slot's
//! generations name is checked against the booted image:
//! - built against its base (`Provenance::base_image_digest`): it merges;
//! - otherwise its declared interface versions (`Provenance::interfaces`)
//!   all match the image's `system-interfaces.toml`: it merges;
//! - otherwise it is held: kept, named in `<root>/held-layers.json`, and
//!   left out of everything merged until a swap brings it back
//!   deliberately or the next image change fits it. The person is told
//!   which layers are held and why (`Apply::publish_notice`, read by the
//!   desktop).
//!
//! `<root>/image-change.json` records the change: the slots that stay on
//! generations made for the previous image, and the held layers.
//!
//! An unknown previous image is never a change: a state root without
//! `image.json` was written by a tilo-vmd that did not record images (on
//! 2026-10-19 such a root counted as one, and the first boot of a recording
//! tilo-vmd dropped every slot's layers and deleted 7.8 GB of them), so it
//! only gets the booted image recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::{Declaration, HeldLayer, ImageChange, Provenance, SlotLayers};

use crate::error::DaemonError;
use crate::fsutil::write_atomic;
use crate::layers::LayerStore;
use crate::slots::{SlotStore, now};

/// The image the last boot ran.
const IMAGE_FILE: &str = "image.json";
/// The last [`ImageChange`].
const CHANGE_FILE: &str = "image-change.json";
const HELD_FILE: &str = "held-layers.json";
/// The notice the desktop shows about held layers.
pub const HELD_NOTICE: &str = "held-layers.json";
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

/// What a layer is checked against on the booted image.
#[derive(Debug, Clone, Default)]
pub struct BootedImage {
    pub id: String,
    /// The base digest layers record (`base.json`'s `image_digest`).
    pub digest: Option<String>,
    /// The image's `[interfaces]` ([`tilo_vm_protocol::SYSTEM_INTERFACES`]).
    pub interfaces: BTreeMap<String, u32>,
    /// The image's own declaration, as recorded for the swap check
    /// ([`crate::interfaces`]); `None` when what `/usr` shows is not the
    /// image's alone (system extensions are merged over it), so it is not
    /// recorded as the image's.
    pub declaration: Option<Declaration>,
}

impl BootedImage {
    /// The image `id` as this machine describes it: `digest` from its
    /// `base.json`, its interfaces from `interfaces_file`.
    #[must_use]
    pub fn read(id: &str, digest: Option<String>, interfaces_file: &Path) -> Self {
        Self {
            id: id.to_owned(),
            digest,
            interfaces: std::fs::read_to_string(interfaces_file)
                .map(|text| tilo_vm_protocol::system_interfaces(&text))
                .unwrap_or_default(),
            declaration: (!usr_is_merged()).then(|| Declaration::read(interfaces_file)),
        }
    }

    /// `None` when a layer with `provenance` merges on this image, else why
    /// it is held.
    #[must_use]
    pub fn hold_reason(&self, provenance: Option<&Provenance>) -> Option<String> {
        let Some(provenance) = provenance else {
            return Some("its provenance is unreadable".into());
        };
        if let (Some(layer), Some(image)) = (&provenance.base_image_digest, &self.digest)
            && layer == image
        {
            return None;
        }
        if provenance.interfaces.is_empty() {
            return Some(match &provenance.base_image_digest {
                Some(base) => format!(
                    "built against another base ({}) and records no interface versions",
                    short(base)
                ),
                None => "records neither its base image nor its interface versions".into(),
            });
        }
        let differing: Vec<String> = provenance
            .interfaces
            .iter()
            .filter(|(name, version)| self.interfaces.get(*name) != Some(*version))
            .map(|(name, version)| match self.interfaces.get(name) {
                Some(served) => format!("{name} {version}, the image serves {served}"),
                None => format!("{name} {version}, the image does not serve it"),
            })
            .collect();
        if differing.is_empty() {
            None
        } else {
            Some(format!("speaks {}", differing.join("; ")))
        }
    }
}

/// Whether system extensions are merged over `/usr` (a `sysext` overlay is
/// mounted there), so a file under it may be a layer's, not the image's.
/// The early-boot unit runs before `systemd-sysext`, and sees the image's.
fn usr_is_merged() -> bool {
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|info| usr_merged_in(&info))
}

fn usr_merged_in(mountinfo: &str) -> bool {
    mountinfo.lines().any(|line| {
        let mut halves = line.splitn(2, " - ");
        let (Some(mount), Some(source)) = (halves.next(), halves.next()) else {
            return false;
        };
        mount.split(' ').nth(4) == Some("/usr") && source.split(' ').nth(1) == Some("sysext")
    })
}

/// The image's own system-interface declaration, as the boot recorded it;
/// `None` when no boot has recorded it yet.
#[must_use]
pub fn recorded_declaration(root: &Path) -> Option<Declaration> {
    read_json::<Recorded>(&root.join(IMAGE_FILE)).and_then(|r| r.interfaces)
}

fn short(digest: &str) -> &str {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    hex.get(..12).unwrap_or(hex)
}

#[derive(Debug, Serialize, Deserialize)]
struct Recorded {
    image: String,
    #[serde(rename = "recorded_at")]
    at: String,
    /// The image's own declaration ([`BootedImage::declaration`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interfaces: Option<Declaration>,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DaemonError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
    write_atomic(path, &bytes)
}

fn remove_if_present(path: &Path, what: &str) -> Result<(), DaemonError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(DaemonError::io(what)(e)),
        _ => Ok(()),
    }
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

#[must_use]
pub fn held(root: &Path) -> Vec<HeldLayer> {
    read_json(&root.join(HELD_FILE)).unwrap_or_default()
}

#[must_use]
pub fn held_hashes(root: &Path) -> BTreeSet<String> {
    held(root).into_iter().map(|h| h.layer).collect()
}

fn write_held(root: &Path, held: &[HeldLayer]) -> Result<(), DaemonError> {
    if held.is_empty() {
        remove_if_present(&root.join(HELD_FILE), "clear the held layers")
    } else {
        write_json(&root.join(HELD_FILE), &held)
    }
}

/// The person brings `layer` back deliberately (a swap names it): it is
/// no longer held.
///
/// # Errors
/// I/O failures.
pub fn release(root: &Path, layer: &str) -> Result<(), DaemonError> {
    let mut held = held(root);
    let before = held.len();
    held.retain(|h| h.layer != layer);
    if held.len() == before {
        return Ok(());
    }
    write_held(root, &held)
}

/// The desktop's notice about held layers: what the person reads, `None`
/// when nothing is held.
#[must_use]
pub fn held_notice(root: &Path) -> Option<String> {
    let held = held(root);
    if held.is_empty() {
        return None;
    }
    let notice = serde_json::json!({
        "image": slots_image(root),
        "layers": held
            .iter()
            .map(|h| serde_json::json!({"component": h.component, "layer": h.layer, "reason": h.reason}))
            .collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&notice).ok()
}

/// Record `booted` as the image the slots run on; see the module
/// documentation. Returns the change when the boot before recorded another
/// image, `None` when it recorded this one or none at all. No slot, no
/// generation and no layer is changed or deleted either way; on a change
/// the layers that do not fit `booted` are held.
///
/// # Errors
/// I/O failures; the image stays unrecorded, so the next boot tries again.
pub fn reconcile(
    root: &Path,
    slots: &SlotStore,
    layers: &LayerStore,
    booted: &BootedImage,
) -> Result<Option<ImageChange>, DaemonError> {
    let recorded: Option<Recorded> = read_json(&root.join(IMAGE_FILE));
    if let Some(same) = recorded.as_ref().filter(|r| r.image == booted.id) {
        // A boot before recorded this image without its declaration (an
        // older tilo-vmd): record it now when it can be read as the image's.
        if same.interfaces.is_none() && booted.declaration.is_some() {
            write_json(
                &root.join(IMAGE_FILE),
                &Recorded {
                    image: same.image.clone(),
                    at: same.at.clone(),
                    interfaces: booted.declaration.clone(),
                },
            )?;
        }
        return Ok(None);
    }
    let at = now();
    let change = match recorded {
        // Nothing says which image the slots were made on: not a change.
        None => None,
        Some(previous) => {
            let mut kept = Vec::new();
            let mut named = BTreeSet::new();
            for record in slots.list()? {
                for number in slots.generations(&record.name) {
                    if let Ok(generation) = slots.generation(&record.name, number) {
                        named.extend(generation.layers);
                    }
                }
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
            let held: Vec<HeldLayer> = named
                .into_iter()
                .filter_map(|hash| {
                    let provenance = layers.provenance(&hash);
                    let reason = booted.hold_reason(provenance.as_ref())?;
                    Some(HeldLayer {
                        component: provenance.map_or_else(|| "?".into(), |p| p.component),
                        layer: hash,
                        reason,
                    })
                })
                .collect();
            write_held(root, &held)?;
            let change = ImageChange {
                from: Some(previous.image),
                to: booted.id.clone(),
                at: at.clone(),
                kept,
                held,
                dropped: Vec::new(),
                freed_bytes: 0,
            };
            write_json(&root.join(CHANGE_FILE), &change)?;
            Some(change)
        }
    };
    remove_if_present(
        &root.join(COLLECT_MARKER),
        "remove the old layer collection marker",
    )?;
    write_json(
        &root.join(IMAGE_FILE),
        &Recorded {
            image: booted.id.clone(),
            at,
            interfaces: booted.declaration.clone(),
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

    fn interfaces(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    /// The booted image: base `sha256:new`, serving shell-ui 10 and runtime 2.
    fn image(id: &str) -> BootedImage {
        BootedImage {
            id: id.to_owned(),
            digest: Some("sha256:new".into()),
            interfaces: interfaces(&[("shell-ui", 10), ("runtime", 2), ("system", 1)]),
            declaration: Some(Declaration::Versions {
                versions: interfaces(&[("shell-ui", 10), ("runtime", 2), ("system", 1)]),
            }),
        }
    }

    fn import_with(layers: &LayerStore, name: &str, provenance: &Provenance) -> String {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("usr/bin")).unwrap();
        std::fs::write(source.path().join("usr/bin").join(name), name).unwrap();
        layers
            .import_path(source.path(), provenance, &mut |_| Ok(()))
            .unwrap()
            .0
    }

    fn import(layers: &LayerStore, name: &str) -> String {
        import_with(layers, name, &Provenance::local(name))
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
    /// image is no change: the image is recorded and nothing else moves,
    /// nothing is held.
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
        std::fs::write(root.path().join(COLLECT_MARKER), b"").unwrap();
        let before = state(&slots, &layers);

        let change = reconcile(root.path(), &slots, &layers, &image("composefs:1c9ab8")).unwrap();

        assert_eq!(change, None, "no recorded image, so no change");
        assert_eq!(last_change(root.path()), None);
        assert_eq!(
            slots_image(root.path()).as_deref(),
            Some("composefs:1c9ab8")
        );
        assert_eq!(state(&slots, &layers), before, "slots and layers untouched");
        assert!(held(root.path()).is_empty(), "nothing held");
        assert!(!root.path().join(COLLECT_MARKER).exists());
    }

    /// Booting another image keeps every slot on its generation and every
    /// layer in the store. A layer built against the booted base merges, one
    /// whose interface versions match the image merges, any other is held:
    /// kept, left out of what merges, named with the reason, and announced.
    /// The same image again changes nothing.
    #[test]
    fn an_image_change_merges_what_fits_and_holds_the_rest_without_deleting() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        assert_eq!(
            reconcile(root.path(), &slots, &layers, &image("composefs:one")).unwrap(),
            None,
            "a fresh machine records its image"
        );
        let mut same_base = Provenance::local("launcher");
        same_base.base_image_digest = Some("sha256:new".into());
        let mut same_interfaces = Provenance::local("install");
        same_interfaces.base_image_digest = Some("sha256:old".into());
        same_interfaces.interfaces = interfaces(&[("shell-ui", 10), ("runtime", 2)]);
        let mut older_interfaces = Provenance::local("shell");
        older_interfaces.interfaces = interfaces(&[("shell-ui", 9), ("runtime", 2)]);
        let launcher = import_with(&layers, "launcher", &same_base);
        let install = import_with(&layers, "install", &same_interfaces);
        let shell = import_with(&layers, "shell", &older_interfaces);
        let greeter = import(&layers, "greeter");
        let device = device_login(&slots);
        for (component, layer) in [
            ("launcher", &launcher),
            ("install", &install),
            ("shell", &shell),
        ] {
            swap(&slots, &device, component, layer, session_info());
        }
        swap(&slots, "vm", "greeter", &greeter, vm_info());
        let before = state(&slots, &layers);

        let change = reconcile(root.path(), &slots, &layers, &image("composefs:two"))
            .unwrap()
            .expect("a change");

        assert_eq!(change.from.as_deref(), Some("composefs:one"));
        assert_eq!(change.to, "composefs:two");
        let kept: Vec<(&str, u64)> = change
            .kept
            .iter()
            .map(|k| (k.slot.as_str(), k.generation))
            .collect();
        assert_eq!(kept, vec![("device", 3), ("vm", 1)]);
        let held_now: BTreeMap<&str, &str> = change
            .held
            .iter()
            .map(|h| (h.component.as_str(), h.reason.as_str()))
            .collect();
        assert_eq!(
            held_now.keys().copied().collect::<Vec<_>>(),
            vec!["greeter", "shell"],
            "{held_now:?}"
        );
        assert!(
            held_now["shell"].contains("shell-ui 9, the image serves 10"),
            "{held_now:?}"
        );
        assert!(
            held_now["greeter"]
                .contains("records neither its base image nor its interface versions"),
            "{held_now:?}"
        );
        assert!(change.dropped.is_empty());
        assert_eq!(
            state(&slots, &layers),
            before,
            "nothing deleted, no slot moved"
        );
        assert_eq!(held(root.path()), change.held);

        // What merges leaves the held layers out, in the generation's order.
        let generation = slots.generation("device", 3).unwrap();
        assert_eq!(
            layers.merged_paths(&generation.layers).unwrap(),
            vec![layers.path(&launcher), layers.path(&install)]
        );
        let notice = held_notice(root.path()).expect("the person is told");
        assert!(notice.contains("shell-ui 9"), "{notice}");
        let said = change.describe().join("\n");
        assert!(said.contains("slot device stays on generation 3"), "{said}");
        assert!(said.contains("held, kept but not merged"), "{said}");

        // The same image again: nothing changes, the holds stay.
        assert_eq!(
            reconcile(root.path(), &slots, &layers, &image("composefs:two")).unwrap(),
            None
        );
        assert_eq!(held(root.path()), change.held);

        release(root.path(), &shell).unwrap();
        assert_eq!(
            layers.merged_paths(&generation.layers).unwrap(),
            vec![
                layers.path(&launcher),
                layers.path(&install),
                layers.path(&shell)
            ]
        );
    }

    #[test]
    fn a_drop_an_older_tilo_vmd_recorded_still_reads_as_one() {
        let record = r#"{"from": null, "to": "composefs:1c9ab8", "at": "2026-10-19T19:32:21Z",
            "dropped": [{"slot": "device", "generation": 106, "components": ["shell"]}],
            "freed_bytes": 7834890240}"#;
        let change: ImageChange = serde_json::from_str(record).unwrap();
        assert!(change.kept.is_empty() && change.held.is_empty());
        let said = change.describe().join("\n");
        assert!(said.contains("an older tilo-vmd dropped"), "{said}");
        assert!(
            said.contains("slot device left generation 106 (shell)"),
            "{said}"
        );
    }

    /// The owner's login slot, as a laptop records it.
    fn device_login(slots: &SlotStore) -> String {
        let apply = crate::apply::RecordingApply::with_login("owner");
        let record = slots
            .load_with(tilo_vm_protocol::DEVICE_SLOT, apply.device)
            .unwrap();
        slots.save(&record).unwrap();
        record.name
    }

    /// The image's own declaration is what the swap check judges the
    /// image's components by. It is recorded with the image, and recorded
    /// later for an image an older tilo-vmd recorded without it; a reading
    /// taken while layers are merged over `/usr` is never recorded as the
    /// image's.
    #[test]
    fn the_image_s_own_declaration_is_recorded_and_a_merged_reading_never_is() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let mut merged = image("composefs:one");
        merged.declaration = None;
        reconcile(root.path(), &slots, &layers, &merged).unwrap();
        assert_eq!(recorded_declaration(root.path()), None);

        let booted = image("composefs:one");
        assert_eq!(
            reconcile(root.path(), &slots, &layers, &booted).unwrap(),
            None,
            "the same image is no change"
        );
        assert_eq!(recorded_declaration(root.path()), booted.declaration);

        reconcile(root.path(), &slots, &layers, &merged).unwrap();
        assert_eq!(
            recorded_declaration(root.path()),
            booted.declaration,
            "a later merged reading keeps the image's"
        );
    }

    #[test]
    fn a_sysext_overlay_on_usr_counts_as_merged() {
        let merged =
            "36 25 0:44 / /usr ro,nodev,relatime shared:20 - overlay sysext ro,lowerdir=/x:/usr\n";
        let image = "25 1 0:33 / / ro,relatime shared:1 - overlay composefs:1c9a ro\n";
        assert!(usr_merged_in(merged));
        assert!(!usr_merged_in(image));
        assert!(!usr_merged_in(
            "40 25 0:50 / /usr/local rw shared:3 - overlay sysext rw\n"
        ));
    }
}
