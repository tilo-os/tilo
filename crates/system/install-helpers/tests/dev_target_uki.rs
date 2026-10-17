//! The development target UKI on real files: the boot representation
//! transition, the sealed and runner-built UKI projections and every refusal
//! before mutation. Only the external `ukify` is a fixture.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use tilo_install_helpers::dev_target_uki::{
    CMDLINE_ROOM, Host, MOUNT, Selected, gate, image_inputs, promote,
};
use tilo_install_helpers::{Error, Result};

const KERNEL: &[u8] = b"exact CI kernel";
const INITRD: &[u8] = b"exact CI initrd";
const MANIFEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ORIGIN: &str = "registry.tilo.so/tilo/tilo-vm:main-fixture-lenovo-83ed";
// What bootc 1.16 `install to-filesystem --composefs-backend` writes ahead of
// the image's kargs in a split-kernel BLS entry (captured from a VM install).
const FS_UUID: &str = "298c49ae-60ed-4a00-a035-c863f0b1a686";
const BOOT_UUID: &str = "adc2062b-7c7c-4f19-960a-ad87b0823796";
const REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");

fn digest() -> String {
    "a".repeat(128)
}

fn bootc_disk_identity() -> String {
    format!("root=UUID={FS_UUID} rootflags=subvol=/ rw boot=UUID={BOOT_UUID}")
}

/// A flattened device tree header and nothing else.
fn dtb() -> Vec<u8> {
    [0xD00D_FEED_u32, 40, 40, 40, 40, 17, 16, 0, 0, 0]
        .iter()
        .flat_map(|word| word.to_be_bytes())
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A minimal ARM64 PE: each section's raw size is its payload, 512-aligned.
fn pe(payloads: &[(&str, &[u8])]) -> Vec<u8> {
    let raw_sizes: Vec<usize> = payloads
        .iter()
        .map(|(_, value)| value.len().div_ceil(512).max(1) * 512)
        .collect();
    let mut data = vec![0u8; 1024 + raw_sizes.iter().sum::<usize>()];
    data[..2].copy_from_slice(b"MZ");
    data[60..64].copy_from_slice(&64u32.to_le_bytes());
    data[64..68].copy_from_slice(b"PE\0\0");
    data[68..70].copy_from_slice(&0xaa64u16.to_le_bytes());
    data[70..72].copy_from_slice(&(payloads.len() as u16).to_le_bytes());
    data[84..86].copy_from_slice(&240u16.to_le_bytes());
    data[86..88].copy_from_slice(&0x2022u16.to_le_bytes());
    let mut raw = 1024;
    for (index, ((name, value), raw_size)) in payloads.iter().zip(&raw_sizes).enumerate() {
        let header = 328 + 40 * index;
        data[header..header + name.len()].copy_from_slice(name.as_bytes());
        for (at, field) in [value.len(), 4096 * (index + 1), *raw_size, raw]
            .iter()
            .enumerate()
        {
            let at = header + 8 + 4 * at;
            data[at..at + 4].copy_from_slice(&(*field as u32).to_le_bytes());
        }
        data[raw..raw + value.len()].copy_from_slice(value);
        raw += raw_size;
    }
    data
}

/// The payload sections of a UKI, each its virtual bytes.
fn sections(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let data = fs::read(path).unwrap();
    tilo_esp_write::uki::section_table(&data)
        .unwrap()
        .into_iter()
        .map(|s| {
            let start = s.raw_offset as usize;
            (
                s.name,
                data[start..start + s.virtual_size as usize].to_vec(),
            )
        })
        .collect()
}

type Build = Box<dyn Fn(&Fixture, &[&str]) -> Result<String>>;

/// An image, an installed target and its ESP in a temporary directory; the
/// image's inputs owned by the test's own user (root in the installer).
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    image: PathBuf,
    target: PathBuf,
    esp: PathBuf,
    kdir: PathBuf,
    dtb: PathBuf,
    origin: PathBuf,
    entry: PathBuf,
    selected: Selected,
    owner: u32,
    transport: RefCell<PathBuf>,
    ukify: Cell<bool>,
    build: RefCell<Option<Build>>,
    builds: Cell<usize>,
}

fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let (image, target, esp) = (root.join("image"), root.join("target"), root.join("esp"));
        let kdir = image.join("usr/lib/modules/7.2.7");
        let digest = digest();
        for (path, data) in [
            (kdir.join("vmlinuz"), KERNEL),
            (kdir.join("initramfs.img"), INITRD),
            (kdir.join("dtb/qcom/board.dtb"), &dtb()[..]),
            (image.join("usr/lib/tilo/image-flavour"), b"dev\n"),
            (image.join("usr/lib/os-release"), b"ID=tilo\n"),
            (
                image.join("usr/lib/systemd/boot/efi/linuxaa64.efi.stub"),
                b"stub",
            ),
            (
                esp.join(format!("EFI/Linux/bootc_composefs-{digest}/vmlinuz")),
                KERNEL,
            ),
            (
                esp.join(format!("EFI/Linux/bootc_composefs-{digest}/initrd")),
                INITRD,
            ),
        ] {
            write(&path, data);
        }
        let deployment = target.join(format!("state/deploy/{digest}"));
        fs::create_dir_all(deployment.join("etc")).unwrap();
        let origin = deployment.join(format!("{digest}.origin"));
        let payload = [KERNEL, INITRD].concat();
        fs::write(
            &origin,
            format!(
                "[origin]\ncontainer-image-reference=ostree-unverified-image:docker://{ORIGIN}\n\
                 [image]\nmanifest_digest={MANIFEST}\n[boot]\nboot_type=bls\ndigest={}\n",
                sha256(&payload)
            ),
        )
        .unwrap();
        let entry = esp.join("loader/entries/bootc-tilo-fixture.conf");
        write(
            &entry,
            format!(
                "title Tilo\nversion {digest}\nsort-key bootc-tilo-0\n\
                 linux /EFI/Linux/bootc_composefs-{digest}/vmlinuz\n\
                 initrd /EFI/Linux/bootc_composefs-{digest}/initrd\n\
                 options {} composefs={digest} systemd.mount-extra=UUID={BOOT_UUID}:/boot:auto:ro \
                 root=tilo-auto quiet tilo.install\n",
                bootc_disk_identity()
            )
            .as_bytes(),
        );
        let dtb_path = kdir.join("dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb");
        fs::write(&dtb_path, dtb()).unwrap();
        fs::copy(
            format!("{REPO}/os/hardware/profiles/lenovo-83ed.json"),
            image.join("usr/lib/tilo/hardware-profile-definition.json"),
        )
        .unwrap();
        fs::write(
            image.join("usr/lib/tilo/hardware-profile.json"),
            serde_json::json!({
                "schema_version": 1, "id": "lenovo-83ed", "architecture": "aarch64",
                "devicetree": format!("/{}", dtb_path.strip_prefix(&image).unwrap().display()),
                "devicetree_sha256": sha256(&dtb()),
                "target_update_ref": "code.tilo.so/tilo/tilo:fixture-lenovo-83ed",
            })
            .to_string(),
        )
        .unwrap();
        // Immutable image metadata: nothing group- or world-writable.
        for entry in walk(&root) {
            let mode = if entry.is_dir() { 0o755 } else { 0o644 };
            fs::set_permissions(&entry, fs::Permissions::from_mode(mode)).unwrap();
        }
        let owner = fs::metadata(&root).unwrap().uid();
        Self {
            _tmp: tmp,
            transport: RefCell::new(root.join("no-transport/target-uki.efi")),
            root,
            image,
            target,
            esp,
            kdir,
            dtb: dtb_path,
            origin,
            entry,
            selected: Selected {
                target_digest: MANIFEST.into(),
                update_origin: Some(ORIGIN.into()),
            },
            owner,
            // Split-kernel cases model the live constructor alongside the
            // fixture build; sealed and runner-built cases turn it off.
            ukify: Cell::new(true),
            build: RefCell::new(Some(Box::new(Self::ukify_build))),
            builds: Cell::new(0),
        }
    }

    fn uki_path(&self) -> PathBuf {
        self.esp
            .join(format!("EFI/Linux/bootc/bootc_composefs-{}.efi", digest()))
    }

    fn promote(&self) -> Result<String> {
        promote(self, &self.image, &self.target, &self.esp, &self.selected)
    }

    fn no_build(&self) {
        *self.build.borrow_mut() = None;
    }

    /// `ukify build` as the fixture: checks the command line it is given and
    /// writes a UKI of the image's payload.
    #[allow(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        reason = "the shape of a fixture constructor"
    )]
    fn ukify_build(&self, argv: &[&str]) -> Result<String> {
        assert_eq!(argv[..2], ["ukify", "build"]);
        let arg = |flag: &str| argv[argv.iter().position(|a| *a == flag).unwrap() + 1];
        let cmdline = fs::read(&arg("--cmdline")[1..]).unwrap();
        let cmdline = String::from_utf8(cmdline).unwrap().trim_end().to_owned();
        let words: Vec<&str> = cmdline.split(' ').collect();
        assert!(!words.contains(&"tilo.install"));
        assert!(words.contains(&MOUNT));
        // bootc's install-time disk identity never reaches the installed UKI.
        let bootc_mount = format!("systemd.mount-extra=UUID={BOOT_UUID}:/boot:auto:ro");
        for word in bootc_disk_identity()
            .split(' ')
            .chain([bootc_mount.as_str()])
        {
            assert!(!words.contains(&word), "{word} in {cmdline}");
        }
        assert!(cmdline.starts_with(&format!("composefs={} root=tilo-auto quiet ", digest())));
        let sealed = [cmdline.as_bytes(), b"\0"].concat();
        fs::write(
            arg("--output"),
            pe(&[
                (".linux", KERNEL),
                (".initrd", INITRD),
                (".cmdline", &sealed),
                (".dtb", &dtb()),
            ]),
        )
        .unwrap();
        Ok(String::new())
    }

    /// The runner's UKI as `os/dev/dev-target-uki.py construct` builds it:
    /// the image's payload with the command-line room reserved.
    fn runner_uki(&self, changes: &[(&str, &[u8])]) -> PathBuf {
        let placeholder = placeholder();
        let reserved = [placeholder.as_bytes(), b"\0"].concat();
        let dtb = dtb();
        let mut payloads: Vec<(&str, &[u8])> = vec![
            (".linux", KERNEL),
            (".initrd", INITRD),
            (".cmdline", &reserved),
            (".dtb", &dtb),
        ];
        for (name, value) in changes {
            payloads.iter_mut().find(|(n, _)| n == name).unwrap().1 = value;
        }
        let output = self.root.join("transport/target-uki.efi");
        write(&output, &pe(&payloads));
        self.transport.borrow_mut().clone_from(&output);
        output
    }
}

fn walk(path: &Path) -> Vec<PathBuf> {
    let mut found = vec![path.to_path_buf()];
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            found.extend(walk(&entry.unwrap().path()));
        }
    }
    found
}

impl Host for Fixture {
    fn command(&self, argv: &[&str]) -> Result<String> {
        self.builds.set(self.builds.get() + 1);
        match &*self.build.borrow() {
            Some(build) => build(self, argv),
            None => Err(Error::new("the fixture has no constructor")),
        }
    }

    fn transport_uki(&self) -> PathBuf {
        self.transport.borrow().clone()
    }

    fn image_owner(&self) -> u32 {
        self.owner
    }

    fn has_ukify(&self) -> bool {
        self.ukify.get()
    }
}

/// The runner builder's reserved token: `CMDLINE_ROOM=` and `x` up to 2047
/// bytes, as `os/dev/dev-target-uki.py` defines them.
fn placeholder() -> String {
    let builder = fs::read_to_string(format!("{REPO}/os/dev/dev-target-uki.py")).unwrap();
    assert!(
        builder.contains(&format!("CMDLINE_ROOM = \"{CMDLINE_ROOM}\"")),
        "the runner must reserve the room the installer writes into"
    );
    assert!(builder.contains("CMDLINE_BYTES = 2047"));
    let token = format!("{CMDLINE_ROOM}=");
    format!("{token}{}", "x".repeat(2047 - token.len()))
}

#[track_caller]
fn refused<T: std::fmt::Debug>(result: Result<T>, message: &str) {
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains(message),
        "{error:?} does not say {message:?}"
    );
}

#[test]
fn runtime_profile_and_pinned_dtb_fail_closed() {
    let fixture = Fixture::new();
    assert_eq!(
        image_inputs(&fixture, &fixture.image).unwrap().dtb,
        Some(fixture.dtb.clone())
    );
    let mut changed = dtb();
    *changed.last_mut().unwrap() = b'X';
    fs::write(&fixture.dtb, changed).unwrap();
    refused(image_inputs(&fixture, &fixture.image), "pinned SHA-256");
}

#[test]
fn conversion_preserves_ci_identity_and_boot_bytes() {
    let fixture = Fixture::new();
    assert_eq!(fixture.promote().unwrap(), digest());
    assert_eq!(fixture.builds.get(), 1);
    let origin = fs::read_to_string(&fixture.origin).unwrap();
    assert_eq!(
        origin,
        format!(
            "[origin]\ncontainer-image-reference = ostree-unverified-image:docker://{ORIGIN}\n\n\
             [image]\nmanifest_digest = {MANIFEST}\n\n\
             [boot]\nboot_type = uki\ndigest = {}\n\n",
            sha256(&[KERNEL, INITRD].concat())
        )
    );
    let entry = fs::read_to_string(&fixture.entry).unwrap();
    assert_eq!(
        entry,
        format!(
            "title Tilo\nversion {0}\nsort-key bootc-tilo-0\nefi /EFI/Linux/bootc/bootc_composefs-{0}.efi\n",
            digest()
        )
    );
    assert_eq!(fs::read(fixture.kdir.join("vmlinuz")).unwrap(), KERNEL);
    assert!(!fixture.image.join("boot").exists());
    assert_eq!(
        fs::metadata(fixture.uki_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    // The scratch directory on the ESP is gone.
    let leftovers: Vec<_> = fs::read_dir(&fixture.esp)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("tilo-dev-target-")
        })
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn sealed_ci_target_projection_retains_origin_and_loader() {
    let fixture = Fixture::new();
    let digest = digest();
    let words = format!("root=tilo-auto composefs={digest}");
    let sealed = [words.as_bytes(), b"\0"].concat();
    let dtb = dtb();
    let original = pe(&[
        (".linux", KERNEL),
        (".initrd", INITRD),
        (".cmdline", &sealed),
        (".dtb", &dtb),
    ]);
    write(&fixture.image.join("boot/EFI/Linux/7.2.7.efi"), &original);
    write(&fixture.uki_path(), &original);
    let origin = fs::read_to_string(&fixture.origin)
        .unwrap()
        .replace("boot_type=bls", "boot_type=uki");
    fs::write(&fixture.origin, origin).unwrap();
    fs::write(
        &fixture.entry,
        format!(
            "title Tilo\nversion {digest}\nsort-key bootc-tilo-0\nefi /EFI/Linux/bootc/bootc_composefs-{digest}.efi\n"
        ),
    )
    .unwrap();
    let (old_origin, old_entry) = (
        fs::read(&fixture.origin).unwrap(),
        fs::read(&fixture.entry).unwrap(),
    );
    fixture.no_build();
    fixture.ukify.set(false);
    assert_eq!(fixture.promote().unwrap(), digest);
    assert_eq!(
        fixture.builds.get(),
        0,
        "a sealed projection runs no constructor"
    );
    assert_eq!(fs::read(&fixture.origin).unwrap(), old_origin);
    assert_eq!(fs::read(&fixture.entry).unwrap(), old_entry);
    assert_eq!(
        fs::read(fixture.image.join("boot/EFI/Linux/7.2.7.efi")).unwrap(),
        original
    );
    let installed = fs::read(fixture.uki_path()).unwrap();
    assert_eq!(
        sections(&fixture.uki_path())[".cmdline"],
        format!("{words} {MOUNT}").as_bytes()
    );
    // Nothing but the command line's bytes and its virtual size changed.
    let room = tilo_esp_write::uki::find_cmdline_room(&original).unwrap();
    let mut unchanged = installed;
    let raw = room.raw_offset..room.raw_offset + room.raw_size;
    unchanged[raw.clone()].copy_from_slice(&original[raw]);
    let size = room.header + 8..room.header + 12;
    unchanged[size.clone()].copy_from_slice(&original[size]);
    assert_eq!(unchanged, original);

    // A full command-line section refuses growth without touching the
    // installed UKI, origin or loader; no constructor fallback is allowed.
    let full = format!("{words} x={}", "z".repeat(512 - words.len() - 3));
    let full = pe(&[
        (".linux", KERNEL),
        (".initrd", INITRD),
        (".cmdline", full.as_bytes()),
        (".dtb", &dtb),
    ]);
    fs::write(fixture.uki_path(), &full).unwrap();
    refused(fixture.promote(), "only 512 are available");
    assert_eq!(fs::read(fixture.uki_path()).unwrap(), full);
    assert_eq!(fs::read(&fixture.origin).unwrap(), old_origin);
    assert_eq!(fs::read(&fixture.entry).unwrap(), old_entry);
}

#[test]
fn split_constructor_missing_fails_during_image_preflight() {
    let fixture = Fixture::new();
    fixture.ukify.set(false);
    refused(
        image_inputs(&fixture, &fixture.image),
        "runtime ukify constructor before installation",
    );
    assert!(!fixture.uki_path().exists());
}

#[test]
fn runner_built_uki_installs_without_a_live_constructor() {
    let fixture = Fixture::new();
    let transported = fixture.runner_uki(&[]);
    let original = fs::read(&transported).unwrap();
    assert_eq!(
        sections(&transported)[".cmdline"],
        [placeholder().as_bytes(), b"\0"].concat()
    );
    fixture.no_build();
    fixture.ukify.set(false);
    image_inputs(&fixture, &fixture.image).unwrap();
    assert_eq!(fixture.promote().unwrap(), digest());
    assert_eq!(fixture.builds.get(), 0, "the live medium has no ukify");
    let installed = sections(&fixture.uki_path());
    assert_eq!(installed[".linux"], KERNEL);
    assert_eq!(installed[".initrd"], INITRD);
    assert_eq!(installed[".dtb"], dtb());
    assert_eq!(
        installed[".cmdline"],
        format!("composefs={} root=tilo-auto quiet {MOUNT}", digest()).as_bytes()
    );
    let origin = fs::read_to_string(&fixture.origin).unwrap();
    assert!(origin.contains("boot_type = uki"));
    assert!(origin.contains(&format!("manifest_digest = {MANIFEST}")));
    assert!(
        fs::read_to_string(&fixture.entry)
            .unwrap()
            .contains(&format!(
                "efi /EFI/Linux/bootc/bootc_composefs-{}.efi",
                digest()
            ))
    );
    assert_eq!(fs::read(&transported).unwrap(), original);
}

#[test]
fn runner_built_uki_must_carry_the_selected_payload_and_room() {
    let fixture = Fixture::new();
    let (entry, origin) = (
        fs::read(&fixture.entry).unwrap(),
        fs::read(&fixture.origin).unwrap(),
    );
    let other_dtb = vec![b'x'; dtb().len()];
    fixture.ukify.set(false);
    for (changes, message) in [
        (
            vec![(".linux", &b"other image kernel"[..])],
            "split kernel payload",
        ),
        (vec![(".dtb", &other_dtb[..])], "pinned hardware DTB"),
        (
            vec![(".cmdline", &b"root=tilo-auto\0"[..])],
            "reserved command-line room",
        ),
    ] {
        let transported = fixture.runner_uki(&changes);
        refused(fixture.promote(), message);
        assert_eq!(fixture.builds.get(), 0);
        assert_eq!(fs::read(&fixture.entry).unwrap(), entry);
        assert_eq!(fs::read(&fixture.origin).unwrap(), origin);
        assert!(!fixture.uki_path().exists());
        fs::remove_dir_all(transported.parent().unwrap()).unwrap();
    }
}

#[test]
fn runner_built_uki_refuses_a_command_line_beyond_its_room() {
    let fixture = Fixture::new();
    let short = format!("{CMDLINE_ROOM}=x");
    fixture.runner_uki(&[(".cmdline", short.as_bytes())]);
    fixture.ukify.set(false);
    let entry = fs::read_to_string(&fixture.entry).unwrap();
    fs::write(
        &fixture.entry,
        entry.replace(
            " tilo.install",
            &format!(" x={} tilo.install", "y".repeat(600)),
        ),
    )
    .unwrap();
    refused(fixture.promote(), "reserved 512");
    assert!(!fixture.uki_path().exists());
}

#[test]
fn runner_built_uki_beside_a_sealed_image_uki_is_ambiguous() {
    let fixture = Fixture::new();
    fixture.runner_uki(&[]);
    write(
        &fixture.image.join("boot/EFI/Linux/7.2.7.efi"),
        &pe(&[
            (".linux", KERNEL),
            (".initrd", INITRD),
            (".cmdline", b"x\0"),
            (".dtb", &dtb()),
        ]),
    );
    refused(
        image_inputs(&fixture, &fixture.image),
        "both a sealed and a transported UKI",
    );
}

#[test]
fn wrong_payload_or_ci_manifest_fails_before_mutation() {
    let digest = digest();
    for variant in [
        "payload",
        "manifest",
        "composefs",
        "ambiguous-kernel",
        "unsafe-bls",
        "other-root",
        "second-bootc-root",
        "no-update-origin",
    ] {
        let mut fixture = Fixture::new();
        let entry = fs::read_to_string(&fixture.entry).unwrap();
        let origin = fs::read(&fixture.origin).unwrap();
        match variant {
            "payload" => fs::write(
                fixture
                    .esp
                    .join(format!("EFI/Linux/bootc_composefs-{digest}/vmlinuz")),
                b"other image kernel",
            )
            .unwrap(),
            "manifest" => fixture.selected.target_digest = format!("sha256:{}", "c".repeat(64)),
            "no-update-origin" => fixture.selected.update_origin = None,
            "composefs" => fs::write(
                &fixture.entry,
                entry.replace(
                    &format!("composefs={digest}"),
                    &format!("composefs={}", "c".repeat(128)),
                ),
            )
            .unwrap(),
            // Only bootc's own UUID root is dropped; any other root stays fatal.
            "other-root" => fs::write(
                &fixture.entry,
                entry.replace(" quiet", " root=/dev/vda3 quiet"),
            )
            .unwrap(),
            "second-bootc-root" => fs::write(
                &fixture.entry,
                entry.replace(" quiet", &format!(" root=UUID={BOOT_UUID} quiet")),
            )
            .unwrap(),
            "ambiguous-kernel" => {
                fs::create_dir(fixture.image.join("usr/lib/modules/other")).unwrap();
            }
            _ => fs::write(
                &fixture.entry,
                entry.replace("linux /EFI/", "linux /../EFI/"),
            )
            .unwrap(),
        }
        assert!(fixture.promote().is_err(), "{variant} was admitted");
        assert_eq!(fixture.builds.get(), 0, "{variant}");
        assert_eq!(fs::read(&fixture.origin).unwrap(), origin, "{variant}");
        assert!(!fixture.uki_path().exists(), "{variant}");
    }
}

#[test]
fn unrelated_mount_survives_and_conflicting_boot_mount_fails_before_writes() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(&fixture.entry).unwrap();
    let unrelated = "systemd.mount-extra=LABEL=DATA:/srv:ext4:ro";
    fs::write(
        &fixture.entry,
        format!("{} {unrelated}\n", original.trim_end_matches('\n')),
    )
    .unwrap();
    fixture.promote().unwrap();
    let cmdline = String::from_utf8(sections(&fixture.uki_path())[".cmdline"].clone()).unwrap();
    assert!(cmdline.split(' ').any(|w| w == unrelated), "{cmdline}");

    // A fresh split-kernel fixture keeps the mount guard apart from the
    // completed transition's already-existing UKI guard.
    let fixture = Fixture::new();
    fs::write(
        &fixture.entry,
        format!(
            "{} systemd.mount-extra=LABEL=OTHER:/boot:ext4:rw\n",
            original.trim_end_matches('\n')
        ),
    )
    .unwrap();
    let (origin, entry) = (
        fs::read(&fixture.origin).unwrap(),
        fs::read(&fixture.entry).unwrap(),
    );
    refused(fixture.promote(), "conflicting /boot");
    assert_eq!(fixture.builds.get(), 0);
    assert_eq!(fs::read(&fixture.origin).unwrap(), origin);
    assert_eq!(fs::read(&fixture.entry).unwrap(), entry);
    assert!(!fixture.uki_path().exists());
}

#[test]
fn empty_efi_is_not_admitted_as_a_sealed_image() {
    let fixture = Fixture::new();
    write(&fixture.image.join("boot/EFI/Linux/7.2.7.efi"), b"");
    refused(
        image_inputs(&fixture, &fixture.image),
        "missing target boot input",
    );
}

#[test]
fn ukify_wrong_dtb_is_rejected_before_boot_metadata_changes() {
    let fixture = Fixture::new();
    let (entry, origin) = (
        fs::read(&fixture.entry).unwrap(),
        fs::read(&fixture.origin).unwrap(),
    );
    *fixture.build.borrow_mut() = Some(Box::new(|fixture: &Fixture, argv: &[&str]| {
        fixture.ukify_build(argv)?;
        let output = argv[argv.iter().position(|a| *a == "--output").unwrap() + 1];
        let data = fs::read(output).unwrap();
        let at = data.windows(dtb().len()).position(|w| w == dtb()).unwrap();
        let mut changed = data;
        changed[at..at + 40].fill(b'x');
        fs::write(output, changed).unwrap();
        Ok(String::new())
    }));
    refused(fixture.promote(), "hardware DTB");
    assert_eq!(fs::read(&fixture.entry).unwrap(), entry);
    assert_eq!(fs::read(&fixture.origin).unwrap(), origin);
    assert!(!fixture.uki_path().exists());
}

/// The VM gate, with this process modelled as root on a machine without
/// the development installer's marker.
struct Root(Fixture);

impl Host for Root {
    fn command(&self, argv: &[&str]) -> Result<String> {
        self.0.command(argv)
    }

    fn is_root(&self) -> bool {
        true
    }

    fn marker(&self) -> PathBuf {
        self.0.root.join("missing-marker")
    }
}

#[test]
fn unsigned_route_requires_real_vm_gate() {
    let host = Root(Fixture::new());
    refused(gate(&host), "development installer VM");
    assert_eq!(host.0.builds.get(), 0);
}

#[test]
fn the_shipped_command_refuses_outside_the_development_vm() {
    let fixture = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_tilo-installer-offline-target");
    let output = Command::new(binary)
        .args(["dev-target-uki", "validate-image", "--image-root"])
        .arg(&fixture.image)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "development target UKI: unsigned target UKI requires the development installer VM\n"
    );
    let output = Command::new(binary)
        .args(["dev-target-uki", "rebuild", "--image-root", "/"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "argument command: invalid choice: 'rebuild' (choose from 'validate-image', 'promote')"
    ));
}
