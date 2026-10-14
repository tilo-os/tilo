//! tilo-extension end to end, with real RPMs and a real P-256 signature.
//!
//! Builds tiny RPMs shaped like the media-codecs packages, publishes a
//! manifest pinning them, signs it with a throwaway P-256 key (openssl, as
//! cosign's KMS signer would) and drives install, activate and remove into a
//! temporary root: real rpm, rpm2archive and `tilo-crypto verify-p256-sha256`.
//! Only what cannot exist off a Tilo machine is replaced: the network is a map
//! of URLs, the base's package list is given, connectivity is "full", and
//! merging is left to `systemd-sysext list` (rootless) rather than done.
//!
//! Needs rpmbuild, rpm, rpm2archive, openssl and a tilo-crypto binary
//! (`$TILO_CRYPTO`, `target/{debug,release}/tilo-crypto` or `PATH`); without
//! them each case says what is missing and passes, as the suite it replaces
//! did. Scratch space lives under `~/.cache/tilo-extension-e2e` (or
//! `$TILO_EXTENSION_E2E_DIR`), never /tmp.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "test helpers fail the test on any I/O error"
)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde_json::{Value, json};
use support::{BASE_PACKAGES, Fake, Machine, RPMFUSION, repo, sha256_hex};

const TOOLS: [&str; 4] = ["rpmbuild", "rpm", "rpm2archive", "openssl"];

struct Package {
    name: &'static str,
    version: &'static str,
    files: &'static [(&'static str, &'static str)],
    links: &'static [(&'static str, &'static str)],
    ld_conf: Option<&'static str>,
}

const PACKAGES: [Package; 4] = [
    Package {
        name: "libavcodec-freeworld",
        version: "7.1.1",
        files: &[(
            "usr/lib64/libavcodec-freeworld/libavcodec.so.61",
            "freeworld libavcodec",
        )],
        links: &[],
        ld_conf: Some("/usr/lib64/libavcodec-freeworld\n"),
    },
    Package {
        name: "gstreamer1-plugin-openh264",
        version: "1.26.5",
        files: &[
            ("usr/lib64/gstreamer-1.0/libgstopenh264.so", "plugin"),
            ("etc/openh264-plugin.conf", "never part of an extension"),
        ],
        links: &[],
        ld_conf: None,
    },
    Package {
        name: "openh264",
        version: "2.6.0",
        files: &[("usr/lib64/libopenh264.so.8", "codec runtime")],
        links: &[],
        ld_conf: None,
    },
    Package {
        name: "mesa-va-drivers-freeworld",
        version: "25.2.4",
        files: &[(
            "usr/lib64/mesa-freeworld/libgallium-25.2.4.so",
            "freeworld gallium",
        )],
        links: &[(
            "usr/lib64/dri-freeworld/radeonsi_drv_video.so",
            "../mesa-freeworld/libgallium-25.2.4.so",
        )],
        ld_conf: Some("/usr/lib64/mesa-freeworld\n"),
    },
];

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

fn find_tilo_crypto() -> Option<PathBuf> {
    let built =
        ["debug", "release"].map(|profile| repo().join("target").join(profile).join("tilo-crypto"));
    let on_path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("tilo-crypto"))
                .collect()
        })
        .unwrap_or_default();
    std::env::var_os("TILO_CRYPTO")
        .map(PathBuf::from)
        .into_iter()
        .chain(built)
        .chain(on_path)
        .find(|c| c.is_file())
}

fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(program).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

struct Fixture {
    _scratch: tempfile::TempDir,
    rpms: BTreeMap<&'static str, PathBuf>,
    key: PathBuf,
    public: PathBuf,
    rogue: PathBuf,
    crypto: PathBuf,
}

fn build_rpm(top: &Path, package: &Package) -> PathBuf {
    let mut install = Vec::new();
    let mut listed = Vec::new();
    for (path, content) in package.files {
        let parent = Path::new(path).parent().unwrap().display();
        install.push(format!("mkdir -p %{{buildroot}}/{parent}"));
        install.push(format!("printf '%s' '{content}' > %{{buildroot}}/{path}"));
        listed.push(format!("/{path}"));
    }
    for (path, target) in package.links {
        let parent = Path::new(path).parent().unwrap().display();
        install.push(format!("mkdir -p %{{buildroot}}/{parent}"));
        install.push(format!("ln -s {target} %{{buildroot}}/{path}"));
        listed.push(format!("/{path}"));
    }
    if let Some(ld_conf) = package.ld_conf {
        install.push("mkdir -p %{buildroot}/etc/ld.so.conf.d".to_owned());
        install.push(format!(
            "printf '%s' '{ld_conf}' > %{{buildroot}}/etc/ld.so.conf.d/{}.conf",
            package.name
        ));
        listed.push(format!("/etc/ld.so.conf.d/{}.conf", package.name));
    }
    let name = package.name;
    let version = package.version;
    let spec = top.join(format!("{name}.spec"));
    fs::write(
        &spec,
        format!(
            "%global debug_package %{{nil}}\n%global __os_install_post %{{nil}}\n%global _build_id_links none\n\
             Name: {name}\nVersion: {version}\nRelease: 1.fc44\nSummary: tilo-extension end-to-end fixture\n\
             License: MIT\nBuildArch: noarch\nAutoReqProv: no\n\n%description\nTest fixture shaped like {name}.\n\n\
             %install\n{}\n\n%files\n{}\n",
            install.join("\n"),
            listed.join("\n")
        ),
    )
    .unwrap();
    // noarch, so the fixtures build on any host; the helper's extraction
    // does not look at an RPM's architecture, only at its name and payload.
    let topdir = format!("_topdir {}", top.display());
    run(
        "rpmbuild",
        &[
            "--define",
            &topdir,
            "--target",
            "noarch",
            "-bb",
            spec.to_str().unwrap(),
        ],
    );
    let built = top
        .join("RPMS/noarch")
        .join(format!("{name}-{version}-1.fc44.noarch.rpm"));
    assert!(built.is_file(), "{} was not built", built.display());
    built
}

/// The RPMs and keys, built once; `None` (with the reason printed) when this
/// machine lacks the tools.
fn fixture() -> Option<&'static Fixture> {
    static FIXTURE: OnceLock<Option<Fixture>> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let crypto = find_tilo_crypto();
            let mut missing: Vec<&str> = TOOLS.into_iter().filter(|tool| !on_path(tool)).collect();
            if crypto.is_none() {
                missing.push("tilo-crypto");
            }
            if !missing.is_empty() {
                eprintln!(
                    "end-to-end extension cases need {}; not run here",
                    missing.join(", ")
                );
                return None;
            }
            let base = std::env::var_os("TILO_EXTENSION_E2E_DIR").map_or_else(
                || {
                    PathBuf::from(std::env::var_os("HOME").unwrap())
                        .join(".cache/tilo-extension-e2e")
                },
                PathBuf::from,
            );
            fs::create_dir_all(&base).unwrap();
            let scratch = tempfile::Builder::new()
                .prefix("run-")
                .tempdir_in(&base)
                .unwrap();
            let top = scratch.path().join("rpmbuild");
            fs::create_dir_all(&top).unwrap();
            let rpms = PACKAGES
                .iter()
                .map(|p| (p.name, build_rpm(&top, p)))
                .collect();
            let key = scratch.path().join("release.key");
            let public = scratch.path().join("release.pub");
            let rogue = scratch.path().join("rogue.key");
            run(
                "openssl",
                &[
                    "ecparam",
                    "-name",
                    "prime256v1",
                    "-genkey",
                    "-noout",
                    "-out",
                    key.to_str().unwrap(),
                ],
            );
            run(
                "openssl",
                &[
                    "ec",
                    "-in",
                    key.to_str().unwrap(),
                    "-pubout",
                    "-out",
                    public.to_str().unwrap(),
                ],
            );
            run(
                "openssl",
                &[
                    "ecparam",
                    "-name",
                    "prime256v1",
                    "-genkey",
                    "-noout",
                    "-out",
                    rogue.to_str().unwrap(),
                ],
            );
            Some(Fixture {
                _scratch: scratch,
                rpms,
                key,
                public,
                rogue,
                crypto: crypto.unwrap(),
            })
        })
        .as_ref()
}

/// What `cosign sign-blob --output-signature` writes: base64 DER.
fn sign(data: &[u8], key: &Path, workdir: &Path) -> Vec<u8> {
    let message = workdir.join("to-sign");
    fs::write(&message, data).unwrap();
    let der = run(
        "openssl",
        &[
            "dgst",
            "-sha256",
            "-sign",
            key.to_str().unwrap(),
            message.to_str().unwrap(),
        ],
    );
    let mut child = Command::new("openssl")
        .args(["base64", "-A"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), &der).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    out.stdout
}

fn machine(fixture: &Fixture) -> Machine {
    let machine = Machine::with_host(Fake {
        real_tools: Some(fixture.crypto.clone()),
        ..Fake::default()
    });
    fs::write(
        machine.root.join("usr/lib/os-release"),
        "ID=fedora\nVERSION_ID=44\n",
    )
    .unwrap();
    fs::copy(&fixture.public, machine.layout.trust_key()).unwrap();
    machine
}

/// Publish the manifest pinning the fixture RPMs, then let `tamper` change
/// the published bytes. Returns the manifest.
fn publish(
    machine: &Machine,
    fixture: &Fixture,
    key: &Path,
    tamper: impl Fn(&Machine, &str, &Value),
) -> Value {
    let fingerprint = tilo_extension::base_packages_sha256(&BASE_PACKAGES);
    let parts = [
        (
            "tilo-codecs",
            vec![
                "libavcodec-freeworld",
                "gstreamer1-plugin-openh264",
                "openh264",
            ],
        ),
        ("tilo-codecs-mesa", vec!["mesa-va-drivers-freeworld"]),
    ];
    let mut manifest_parts = Vec::new();
    for (part, names) in parts {
        let mut artifacts = Vec::new();
        for name in names {
            let path = &fixture.rpms[name];
            let data = fs::read(path).unwrap();
            let file = path.file_name().unwrap().to_str().unwrap();
            let url = format!("{RPMFUSION}/{}/{file}", &name[..1]);
            artifacts.push(json!({
                "package": file.strip_suffix(".rpm").unwrap(), "urls": [url], "size": data.len(),
                "sha256": sha256_hex(&data), "publisher": "RPM Fusion", "license": "MIT",
            }));
            machine.host.web.borrow_mut().insert(url, data);
        }
        manifest_parts.push(json!({"name": part, "format": "rpm", "state": "available",
                                   "requires_base": {}, "artifacts": artifacts}));
    }
    let manifest = json!({
        "type": tilo_extension::MANIFEST_TYPE, "schema": 1, "id": "media-codecs", "revision": 20_261_003_120_000_u64,
        "created": "2026-10-03T12:00:00Z",
        "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64", "packages_sha256": fingerprint},
        "parts": manifest_parts,
    });
    let data = serde_json::to_vec_pretty(&manifest).unwrap();
    let url = machine.manifest_url(Some(&fingerprint));
    let signature = sign(&data, key, &machine.root);
    machine
        .host
        .web
        .borrow_mut()
        .insert(format!("{url}.sig"), signature);
    machine.host.web.borrow_mut().insert(url.clone(), data);
    tamper(machine, &url, &manifest);
    manifest
}

fn install(machine: &Machine) -> (i32, Value) {
    let code = machine.run("install");
    (code, machine.status())
}

#[test]
fn install_assembles_signed_artifacts_and_remove_restores_everything() {
    let Some(fixture) = fixture() else { return };
    let machine = machine(fixture);
    publish(&machine, fixture, &fixture.key, |_, _, _| {});
    let (code, status) = install(&machine);
    assert_eq!(code, 0, "{status}");
    assert_eq!(
        (status["state"].clone(), status["parts"].clone()),
        (
            json!("installed"),
            json!({"tilo-codecs": "enabled", "tilo-codecs-mesa": "enabled"})
        )
    );
    let codecs = machine.codecs();
    assert_eq!(
        fs::read_to_string(codecs.join("usr/lib64/gstreamer-1.0/libgstopenh264.so")).unwrap(),
        "plugin"
    );
    assert!(
        !codecs.join("etc").exists(),
        "an extension carries only /usr"
    );
    assert_eq!(
        fs::read_to_string(codecs.join("usr/lib64/libavcodec.so.61")).unwrap(),
        "freeworld libavcodec"
    );
    let mesa = machine.part("tilo-codecs-mesa");
    assert_eq!(
        fs::read_to_string(mesa.join("usr/lib64/dri-freeworld/radeonsi_drv_video.so")).unwrap(),
        "freeworld gallium"
    );
    if on_path("systemd-sysext") {
        let listing = String::from_utf8(run(
            "systemd-sysext",
            &[&format!("--root={}", machine.root.display()), "list"],
        ))
        .unwrap();
        for name in ["tilo-codecs", "tilo-codecs-mesa"] {
            assert!(
                listing
                    .lines()
                    .any(|l| l.split_whitespace().take(2).eq([name, "directory"])),
                "{listing}"
            );
        }
    }
    // Whatever is written to /var later is replaced by what the signed
    // manifest pins, the next time the parts are activated.
    fs::write(codecs.join("usr/lib64/libavcodec.so.61"), "hijacked").unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(
        fs::read_to_string(codecs.join("usr/lib64/libavcodec.so.61")).unwrap(),
        "freeworld libavcodec"
    );
    assert_eq!(machine.run("remove"), 0);
    assert_eq!(machine.status()["state"], "not-installed");
    assert_eq!(
        fs::read_dir(machine.layout.extensions()).unwrap().count(),
        0
    );
    assert!(!machine.layout.objects("media-codecs").exists());
}

fn refused(
    fragment: &str,
    key: impl Fn(&Fixture) -> &Path,
    tamper: impl Fn(&Machine, &str, &Value),
) {
    let Some(fixture) = fixture() else { return };
    let machine = machine(fixture);
    publish(&machine, fixture, key(fixture), tamper);
    let (code, status) = install(&machine);
    assert_eq!(code, 1);
    assert!(
        status["last_error"].as_str().unwrap().contains(fragment),
        "{status}"
    );
    assert_eq!(status["state"], "failed");
    assert!(!machine.codecs().exists());
}

#[test]
fn a_manifest_signed_by_another_key_is_refused() {
    refused("signature is not valid", |f| &f.rogue, |_, _, _| {});
}

#[test]
fn a_manifest_changed_after_signing_is_refused() {
    refused(
        "signature is not valid",
        |f| &f.key,
        |machine, url, _| {
            let mut web = machine.host.web.borrow_mut();
            let text = String::from_utf8(web[url].clone()).unwrap();
            web.insert(
                url.to_owned(),
                text.replacen(r#""license": "MIT""#, r#""license": "MIU""#, 1)
                    .into_bytes(),
            );
        },
    );
}

#[test]
fn a_replaced_artifact_is_refused() {
    refused(
        "does not match the signed manifest",
        |f| &f.key,
        |machine, _, manifest| {
            let url = manifest["parts"][0]["artifacts"][0]["urls"][0]
                .as_str()
                .unwrap();
            let mut web = machine.host.web.borrow_mut();
            let last = web[url].len() - 1;
            web.get_mut(url).unwrap()[last] ^= 1;
        },
    );
}
