//! The installed hardware profile is read as `hardware_profile.py` reads it:
//! only through root-owned, unwritable, symlink-free paths, with the
//! definition validated and the update origin bound, and only an official
//! release image of the served board is applicable.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tilo_board_successor::installed_profile;
use tilo_board_successor::profile::{
    DEFAULT_TARGET_IMGREF, Root, development_origin, install_update_origin, read_profile,
    resolve_profile, validate_definition,
};

const PROFILES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../os/hardware/profiles");
const DEVELOPMENT_REF: &str = "registry.tilo.so/tilo/tilo-development:aarch64";

fn shipped(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(format!("{PROFILES}/{name}.json")).unwrap()).unwrap()
}

fn release_tag() -> String {
    format!("code.tilo.so/tilo/tilo:lenovo-83ed-{}", "1".repeat(40))
}

/// An image root in a temporary directory, its inputs owned by the test's
/// own user (root in the image).
struct Image {
    _work: tempfile::TempDir,
    root: Root,
}

impl Image {
    fn new() -> Self {
        let work = tempfile::tempdir().unwrap();
        fs::set_permissions(work.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let owner = fs::metadata(work.path()).unwrap().uid();
        let root = Root {
            path: work.path().to_path_buf(),
            owner,
        };
        Self { _work: work, root }
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.root.path.join(relative)
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.path(relative);
        let mut directory = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&directory).unwrap();
        while directory != self.root.path {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
            directory.pop();
        }
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn json(&self, relative: &str, value: &Value) {
        self.write(relative, value.to_string().as_bytes());
    }

    fn board(&self, definition: &Value, target: &str) {
        self.json("usr/lib/tilo/hardware-profile-definition.json", definition);
        self.json(
            "usr/lib/tilo/hardware-profile.json",
            &json!({"schema_version": 1, "id": definition["id"], "architecture": definition["architecture"],
                    "devicetree": definition["devicetree"], "devicetree_sha256": "2".repeat(64),
                    "target_update_ref": target}),
        );
    }

    fn lenovo(target: &str) -> Self {
        let image = Self::new();
        image.board(&shipped("lenovo-83ed"), target);
        image
    }

    fn profile(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("usr/lib/tilo/hardware-profile.json")).unwrap())
            .unwrap()
    }

    fn set_profile(&self, key: &str, value: Value) {
        let mut profile = self.profile();
        profile[key] = value;
        self.json("usr/lib/tilo/hardware-profile.json", &profile);
    }
}

#[test]
fn an_image_without_a_profile_is_not_a_board() {
    let image = Image::new();
    assert_eq!(read_profile(&image.root).unwrap(), None);
    assert_eq!(installed_profile(&image.root).unwrap(), None);
    image.json(
        "usr/lib/tilo/hardware-profile-definition.json",
        &shipped("lenovo-83ed"),
    );
    assert!(
        read_profile(&image.root).is_err(),
        "a definition needs its generated profile"
    );
}

#[test]
fn an_official_board_release_is_applicable() {
    for target in [
        release_tag(),
        format!("code.tilo.so/tilo/tilo@sha256:{}", "3".repeat(64)),
    ] {
        let image = Image::lenovo(&target);
        let profile = installed_profile(&image.root).unwrap().unwrap();
        assert_eq!(profile["target_update_ref"], json!(target));
    }
}

#[test]
fn a_board_following_anything_but_an_immutable_release_is_refused() {
    for target in [
        "registry.tilo.so/tilo/tilo:latest",
        "code.tilo.so/tilo/tilo:latest",
    ] {
        let image = Image::lenovo(target);
        assert!(read_profile(&image.root).unwrap().is_some(), "{target}");
        assert!(installed_profile(&image.root).is_err(), "{target}");
    }
    for target in [
        "code.tilo.so/tilo/tilo",
        "example.com/tilo/tilo:latest",
        "code.tilo.so/tilo/tilo:",
    ] {
        let image = Image::lenovo(target);
        assert!(read_profile(&image.root).is_err(), "{target}");
    }
}

#[test]
fn another_board_is_not_applicable() {
    let image = Image::new();
    image.board(
        &shipped("orangepi-3b-v2-1"),
        "registry.tilo.so/tilo/tilo:latest",
    );
    assert!(read_profile(&image.root).unwrap().is_some());
    assert_eq!(installed_profile(&image.root).unwrap(), None);
}

#[test]
fn profile_fields_schema_and_definition_binding_are_exact() {
    let mutations: Vec<(&str, Value)> = vec![
        ("schema_version", json!(2)),
        ("schema_version", json!(true)),
        ("schema_version", json!(1.0)),
        ("id", json!("orangepi-3b-v2-1")),
        ("architecture", json!("x86_64")),
        ("unknown", json!(1)),
        ("target_update_ref", json!(5)),
    ];
    for (key, value) in mutations {
        let image = Image::lenovo(&release_tag());
        image.set_profile(key, value.clone());
        assert!(read_profile(&image.root).is_err(), "{key} = {value}");
    }
    let image = Image::lenovo(&release_tag());
    let mut profile = image.profile();
    profile.as_object_mut().unwrap().remove("devicetree_sha256");
    image.json("usr/lib/tilo/hardware-profile.json", &profile);
    assert!(read_profile(&image.root).is_err(), "a missing field");
    let image = Image::lenovo(&release_tag());
    image.write("usr/lib/tilo/hardware-profile.json", &vec![b' '; 16385]);
    assert!(read_profile(&image.root).is_err(), "an oversized profile");
    let image = Image::lenovo(&release_tag());
    image.write("usr/lib/tilo/hardware-profile.json", b"{");
    assert!(
        read_profile(&image.root).is_err(),
        "a profile that is not JSON"
    );
}

#[test]
fn profile_inputs_must_be_trusted_paths() {
    let image = Image::lenovo(&release_tag());
    fs::set_permissions(
        image.path("usr/lib/tilo"),
        fs::Permissions::from_mode(0o775),
    )
    .unwrap();
    assert!(
        read_profile(&image.root).is_err(),
        "a group-writable directory"
    );

    let image = Image::lenovo(&release_tag());
    fs::set_permissions(
        image.path("usr/lib/tilo/hardware-profile.json"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(
        read_profile(&image.root).is_err(),
        "a world-writable profile"
    );

    let image = Image::lenovo(&release_tag());
    let real = image.path("usr/lib/real-tilo");
    fs::rename(image.path("usr/lib/tilo"), &real).unwrap();
    symlink("real-tilo", image.path("usr/lib/tilo")).unwrap();
    assert!(read_profile(&image.root).is_err(), "a symlinked directory");

    let image = Image::lenovo(&release_tag());
    let profile = image.path("usr/lib/tilo/hardware-profile.json");
    fs::rename(&profile, image.path("usr/lib/tilo/elsewhere.json")).unwrap();
    symlink("elsewhere.json", &profile).unwrap();
    assert!(read_profile(&image.root).is_err(), "a symlinked profile");

    let image = Image::lenovo(&release_tag());
    let definition = image.path("usr/lib/tilo/hardware-profile-definition.json");
    fs::remove_file(&definition).unwrap();
    fs::create_dir(&definition).unwrap();
    fs::set_permissions(&definition, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        read_profile(&image.root).is_err(),
        "a definition that is a directory"
    );

    let foreign = Root {
        path: image.root.path.clone(),
        owner: image.root.owner.wrapping_add(1),
    };
    assert!(
        read_profile(&foreign).is_err(),
        "inputs owned by someone else"
    );
}

#[test]
fn shipped_definitions_validate() {
    for name in ["lenovo-83ed", "orangepi-3b-v2-1"] {
        assert!(validate_definition(shipped(name)).is_ok(), "{name}");
    }
    let minimal = json!({"schema_version": 1, "id": "board", "architecture": "x86_64",
                         "devicetree": null, "firmware": {}});
    assert!(validate_definition(minimal).is_ok());
}

#[test]
fn definition_mutations_are_refused() {
    let lenovo = shipped("lenovo-83ed");
    let mutations: Vec<(&str, Value)> = vec![
        ("schema_version", json!(3)),
        ("schema_version", json!(true)),
        ("id", json!("generic")),
        ("id", json!("Lenovo")),
        ("architecture", json!("riscv64")),
        ("devicetree", json!("qcom/board.dts")),
        ("devicetree", json!("../board.dtb")),
        ("devicetree", json!("qcom//board.dtb")),
        ("firmware", json!({"a/b.mbn": "qcom/x.mbn"})),
        (
            "firmware",
            json!({"a.mbn": "qcom/x.mbn", "b.mbn": "qcom/x.mbn/y"}),
        ),
        ("firmware", json!([])),
        ("system_firmware", json!(["qcom/x1e80100/LENOVO/83ED"])),
        ("system_firmware", json!(["regulatory.db", "regulatory.db"])),
        (
            "firmware_exclude",
            json!(["nvidia-gpu-firmware", "nvidia-gpu-firmware"]),
        ),
        ("firmware_exclude", json!(["linux-firmware"])),
        ("firmware_exclude", json!([1])),
        ("boot_modules", json!(["ath12k", "ath12k"])),
        ("boot_modules", json!(["Ath12k"])),
        ("always_on", json!(1)),
        ("wifi_wake_on_wlan", json!(1)),
        ("wifi_wake_on_wlan", json!(false)),
        ("kernel_args", json!(["quiet", "quiet"])),
        ("kernel_args", json!(["a b"])),
        (
            "internal_keyboards",
            json!([{"name": "kb", "bus": "usb", "vendor": "048D",
                                        "product": "8987", "devicetree_compatible": "lenovo,yoga"}]),
        ),
        (
            "internal_keyboards",
            json!([{"name": "kb", "bus": "i2c", "vendor": "048d",
                                        "product": "8987", "devicetree_compatible": "lenovo,yoga"}]),
        ),
        ("unknown", json!(1)),
    ];
    for (key, value) in mutations {
        let mut definition = lenovo.clone();
        definition[key] = value.clone();
        assert!(validate_definition(definition).is_err(), "{key} = {value}");
    }
    let mut excluded = shipped("orangepi-3b-v2-1");
    excluded["system_firmware"] = json!(["brcm/brcmfmac43456-sdio.bin"]);
    excluded["firmware_exclude"] = json!(["brcmfmac-firmware"]);
    assert!(
        validate_definition(excluded).is_err(),
        "removing firmware the board names"
    );
    let mut wake = shipped("orangepi-3b-v2-1");
    wake["wifi_wake_on_wlan"] = json!(0);
    assert!(
        validate_definition(wake).is_err(),
        "the wake policy is the Lenovo board's"
    );
    let mut schema1 = lenovo;
    schema1["schema_version"] = json!(1);
    assert!(
        validate_definition(schema1).is_err(),
        "schema 1 has no optional fields"
    );
}

#[test]
fn a_tilo_vm_origin_needs_the_dev_flavour_and_its_repository_policy() {
    let target = "registry.tilo.so/tilo/tilo-vm:latest";
    let insecure = json!({"transports": {"docker": {"registry.tilo.so/tilo/tilo-vm": [{"type": "insecureAcceptAnything"}]}}});
    let signed = json!({"transports": {"docker": {"registry.tilo.so/tilo/tilo-vm": [{"type": "sigstoreSigned",
        "keyPath": "/usr/share/tilo/cosign.pub", "signedIdentity": {"type": "matchRepository"}}]}}});
    let image = Image::lenovo(target);
    assert!(read_profile(&image.root).is_err(), "no flavour, no policy");
    image.write("usr/lib/tilo/image-flavour", b"dev\n");
    image.json("etc/containers/policy.json", &insecure);
    assert!(read_profile(&image.root).unwrap().is_some());
    assert!(
        installed_profile(&image.root).is_err(),
        "a tilo-vm origin is not an official release"
    );
    image.write("usr/lib/tilo/image-flavour", b"release\n");
    assert!(read_profile(&image.root).is_err(), "the release flavour");
    image.write("usr/lib/tilo/image-flavour", b"dev\n");
    image.json("etc/containers/policy.json", &signed);
    assert!(
        read_profile(&image.root).is_err(),
        "signed admission needs the key"
    );
    image.write("usr/share/tilo/cosign.pub", b"key\n");
    assert!(read_profile(&image.root).unwrap().is_some());
    image.json(
        "etc/containers/policy.json",
        &json!({"transports": {"docker": {}}}),
    );
    assert!(
        read_profile(&image.root).is_err(),
        "no admission for the repository"
    );
    image.json("etc/containers/policy.json", &json!([]));
    assert!(
        read_profile(&image.root).is_err(),
        "a policy that is not an object"
    );
}

type Tamper<'a> = Box<dyn Fn(&Image) + 'a>;

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A development image bound to `DEVELOPMENT_REF` with every trust input
/// consistent.
fn development_image() -> Image {
    let image = Image::lenovo(DEVELOPMENT_REF);
    let dev = "usr/share/tilo/development-trust";
    let identities: [(&str, &[u8]); 4] = [
        ("cosign.pub", b"cosign public key\n"),
        ("tpm2-pcr-signing.public.pem", b"pcr public key\n"),
        ("efi-signing.crt", b"efi certificate\n"),
        (
            "registry-owner.pub",
            b"abababababababababababababababababababababababababababababababab\n",
        ),
    ];
    let mut digests = serde_json::Map::new();
    for (name, bytes) in identities {
        image.write(&format!("{dev}/{name}"), bytes);
        digests.insert(name.to_owned(), json!(sha256(bytes)));
    }
    image.write("usr/share/tilo/cosign.pub", identities[0].1);
    image.write(
        "usr/share/tilo/tpm2-pcr-signing.public.pem",
        identities[1].1,
    );
    image.write(
        "usr/share/tilo/trust/tilo-main-registry-owner.pub",
        identities[3].1,
    );
    let manifest =
        json!({"schema": 1, "purpose": "tilo-development", "sha256": digests}).to_string();
    image.write(&format!("{dev}/manifest.json"), manifest.as_bytes());
    let bound = sha256(manifest.as_bytes());
    image.json(
        &format!("{dev}/origin.json"),
        &json!({"schema": 1, "purpose": "tilo-development", "target_update_ref": DEVELOPMENT_REF, "manifest_sha256": bound}),
    );
    image.json(
        &format!("{dev}/provenance.json"),
        &json!({"purpose": "tilo-development", "manifest_sha256": bound}),
    );
    let repository = "registry.tilo.so/tilo/tilo-development";
    image.json(
        "etc/containers/policy.json",
        &json!({"default": [{"type": "reject"}], "transports": {"docker": {repository: [
            {"type": "sigstoreSigned", "keyPath": "/usr/share/tilo/cosign.pub",
             "signedIdentity": {"type": "matchRepository"}}]}}}),
    );
    image.write(
        "etc/containers/registries.d/tilo-development.yaml",
        format!("docker:\n  \"{repository}\":\n    use-sigstore-attachments: true\n").as_bytes(),
    );
    image.write(
        "etc/containers/registries.conf.d/99-tilo-development.conf",
        b"[[registry]]\nlocation = \"registry.tilo.so\"\ninsecure = false\n",
    );
    image
}

#[test]
fn a_bound_development_image_is_read_but_never_applicable() {
    let image = development_image();
    assert_eq!(
        development_origin(&image.root).unwrap(),
        Some(json!(DEVELOPMENT_REF))
    );
    assert!(read_profile(&image.root).unwrap().is_some());
    assert_eq!(installed_profile(&image.root).unwrap(), None);
    image.set_profile("target_update_ref", json!(release_tag()));
    assert!(
        read_profile(&image.root).is_err(),
        "the profile must follow the bound origin"
    );
}

#[test]
fn every_development_trust_input_is_bound() {
    let dev = "usr/share/tilo/development-trust";
    let tampers: Vec<(&str, Tamper<'_>)> = vec![
        (
            "an identity",
            Box::new(|i: &Image| i.write(&format!("{dev}/efi-signing.crt"), b"other\n")),
        ),
        (
            "the installed key",
            Box::new(|i: &Image| i.write("usr/share/tilo/cosign.pub", b"other\n")),
        ),
        (
            "the installed owner",
            Box::new(|i: &Image| {
                i.write(
                    "usr/share/tilo/trust/tilo-main-registry-owner.pub",
                    &[b'c'; 64],
                );
            }),
        ),
        (
            "the provenance",
            Box::new(|i: &Image| {
                i.json(&format!("{dev}/provenance.json"), &json!({"purpose": "x"}));
            }),
        ),
        (
            "the policy",
            Box::new(|i: &Image| {
                i.json(
                    "etc/containers/policy.json",
                    &json!({"default": [{"type": "reject"}]}),
                );
            }),
        ),
        (
            "the transport",
            Box::new(|i: &Image| {
                i.write(
                    "etc/containers/registries.conf.d/99-tilo-development.conf",
                    b"[[registry]]\nlocation = \"registry.tilo.so\"\ninsecure = true\n",
                );
            }),
        ),
        (
            "the origin",
            Box::new(|i: &Image| {
                let path = i.path(&format!("{dev}/origin.json"));
                let mut origin: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                origin["target_update_ref"] = json!("dev.example.com/tilo-development/os:aarch64");
                i.json(&format!("{dev}/origin.json"), &origin);
            }),
        ),
        (
            "a missing identity",
            Box::new(|i: &Image| fs::remove_file(i.path(&format!("{dev}/cosign.pub"))).unwrap()),
        ),
        (
            "an oversized input",
            Box::new(|i: &Image| i.write(&format!("{dev}/provenance.json"), &vec![b' '; 65537])),
        ),
    ];
    for (what, tamper) in tampers {
        let image = development_image();
        tamper(&image);
        assert!(development_origin(&image.root).is_err(), "{what}");
        assert!(installed_profile(&image.root).is_err(), "{what}");
    }
}

#[test]
fn the_reader_tracks_the_shipped_module() {
    // The image builders still read profiles through hardware_profile.py; these are the module's own
    // refusals the Rust reader mirrors word for word.
    let module =
        fs::read_to_string(Path::new(PROFILES).join("../../dev/hardware_profile.py")).unwrap();
    for message in [
        "hardware profile has missing or unknown fields",
        "hardware profile id does not match its image-owned definition",
        "development origin public manifest binding differs",
        "installed development registry owner differs from bound public identity",
        "tilo-vm update origin requires its development repository policy",
        "hardware definition firmware_exclude names a package that may not be removed",
        "TILO_TARGET_IMGREF conflicts with the image-owned hardware profile update reference",
        DEFAULT_TARGET_IMGREF,
    ] {
        assert!(module.contains(message), "{message}");
    }
}

#[test]
fn the_profile_resolves_to_its_pinned_kernel_dtb() {
    // A flattened device tree header: magic, then the total size.
    let mut dtb = vec![0u8; 40];
    dtb[..4].copy_from_slice(&0xD00D_FEEDu32.to_be_bytes());
    dtb[4..8].copy_from_slice(&40u32.to_be_bytes());
    let image = Image::lenovo("registry.tilo.so/tilo/tilo:latest");
    let definition = shipped("lenovo-83ed");
    let relative = format!(
        "usr/lib/modules/7.2.7/dtb/{}",
        definition["devicetree"].as_str().unwrap()
    );
    image.write(&relative, &dtb);
    image.set_profile("devicetree", json!(format!("/{relative}")));
    image.set_profile(
        "devicetree_sha256",
        json!(hex::encode(Sha256::digest(&dtb))),
    );
    assert_eq!(
        resolve_profile(&image.root, "7.2.7", "aarch64").unwrap(),
        Some(image.path(&relative))
    );
    let refused = |kver: &str, architecture: &str| {
        resolve_profile(&image.root, kver, architecture)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        refused("../7", "aarch64"),
        "invalid kernel version for hardware profile"
    );
    assert_eq!(
        refused("7.2.7", "x86_64"),
        "hardware profile requires a matching native image architecture"
    );
    assert_eq!(
        refused("7.2.8", "aarch64"),
        "hardware profile device tree does not match this kernel and model"
    );
    let mut changed = dtb.clone();
    changed[39] = 1;
    image.write(&relative, &changed);
    assert_eq!(
        refused("7.2.7", "aarch64"),
        "hardware profile device tree does not match its pinned SHA-256"
    );
    image.write(&relative, &changed[..39]);
    assert_eq!(
        refused("7.2.7", "aarch64"),
        "hardware profile device tree has an invalid flattened-device-tree header"
    );
    assert_eq!(
        resolve_profile(&Image::new().root, "7.2.7", "aarch64").unwrap(),
        None
    );
}

#[test]
fn an_install_records_the_profile_origin_or_a_validated_override() {
    let generic = Image::new();
    assert_eq!(
        install_update_origin(&generic.root, "").unwrap(),
        DEFAULT_TARGET_IMGREF
    );
    let pinned = "registry.tilo.so/tilo/tilo:gen7";
    assert_eq!(
        install_update_origin(&generic.root, pinned).unwrap(),
        pinned
    );
    for refused in [
        "registry.tilo.so/tilo/tilo",
        "example.com/tilo/tilo:latest",
        "registry.tilo.so/tilo/tilo-vm:latest",
    ] {
        assert!(
            install_update_origin(&generic.root, refused).is_err(),
            "{refused}"
        );
    }

    let board = Image::lenovo(&release_tag());
    assert_eq!(
        install_update_origin(&board.root, "").unwrap(),
        release_tag()
    );
    assert_eq!(
        install_update_origin(&board.root, &release_tag()).unwrap(),
        release_tag()
    );
    assert_eq!(
        install_update_origin(&board.root, pinned)
            .unwrap_err()
            .to_string(),
        "TILO_TARGET_IMGREF conflicts with the image-owned hardware profile update reference"
    );

    let development = development_image();
    assert_eq!(
        install_update_origin(&development.root, "").unwrap(),
        DEVELOPMENT_REF
    );
    fs::remove_file(development.path("usr/lib/tilo/hardware-profile.json")).unwrap();
    fs::remove_file(development.path("usr/lib/tilo/hardware-profile-definition.json")).unwrap();
    assert_eq!(
        install_update_origin(&development.root, "").unwrap(),
        DEVELOPMENT_REF,
        "a development image without a profile follows its bound origin"
    );
    assert!(install_update_origin(&development.root, pinned).is_err());
}

#[test]
fn the_installer_command_prints_the_origin_of_the_running_image() {
    let run = |override_ref: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_tilo-board-successor"))
            .args(["install-update-origin", override_ref])
            .output()
            .unwrap()
    };
    let output = run("");
    match install_update_origin(&Root::system(), "") {
        Ok(origin) => {
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{origin}\n")
            );
        }
        Err(error) => {
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!("hardware profile rejected: {error}\n")
            );
        }
    }
    let refused = run("example.com/tilo/tilo:latest");
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8(refused.stderr)
            .unwrap()
            .starts_with("hardware profile rejected: "),
    );
}
