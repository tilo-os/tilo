//! tilo-extension end to end, with real RPMs and real signatures.
//!
//! Builds tiny RPMs shaped like the media-codecs packages, publishes a
//! manifest pinning them, signs it with a throwaway P-256 key (openssl, as
//! cosign's KMS signer would) and drives install, activate and remove into a
//! temporary root: real rpm, rpm2archive and `tilo-crypto verify-p256-sha256`
//! (or, on an image with the release root, `verify-release-bundle` over a
//! certificate from the real ceremony script, tools/signing/release-ca.sh, and
//! a log entry from a throwaway Rekor key).
//! Only what cannot exist off a Tilo machine is replaced: the network is a map
//! of URLs, the base's package list is given, connectivity is "full", and
//! merging is left to `systemd-sysext list` (rootless) rather than done.
//!
//! Needs rpmbuild, rpm, rpm2archive, openssl and systemd-sysext, which the
//! build image carries, and the tilo-crypto binary the run builds beside this
//! package's (t.toml `starts`). Scratch space lives under
//! `~/.cache/tilo-extension-e2e` (or `$TILO_EXTENSION_E2E_DIR`), never /tmp.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test helpers fail the test on any I/O error"
)]
#![allow(
    clippy::too_many_lines,
    clippy::panic,
    reason = "one journey asserts every step, and a missing tool fails it by name"
)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

use serde_json::json;
use support::{BASE_PACKAGES, Fake, Machine, RPMFUSION, repo, sha256_hex};

fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(program)
        .args(args)
        .output_locked()
        .unwrap_or_else(|error| panic!("{program} could not start: {error}"));
    assert!(
        out.status.success(),
        "{program} {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn openssl_stdin(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn_locked()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    out.stdout
}

fn base64(data: &[u8]) -> String {
    String::from_utf8(openssl_stdin(&["base64", "-A"], data)).unwrap()
}

fn p256_key(path: &Path) {
    let path = path.to_str().unwrap();
    run(
        "openssl",
        &[
            "ecparam",
            "-name",
            "prime256v1",
            "-genkey",
            "-noout",
            "-out",
            path,
        ],
    );
}

fn public_key(key: &Path, public: &Path) {
    let (key, public) = (key.to_str().unwrap(), public.to_str().unwrap());
    run("openssl", &["ec", "-in", key, "-pubout", "-out", public]);
}

/// A base64 DER ECDSA signature over SHA-256, as cosign's signer makes one.
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
    base64(&der).into_bytes()
}

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

/// Keys, the release roots and certificate the ceremony makes, and a Rekor key.
struct Keys {
    _scratch: tempfile::TempDir,
    dir: PathBuf,
}

impl Keys {
    fn new() -> Self {
        // rpmbuild's tree is large for a small /tmp.
        let base = std::env::var_os("TILO_EXTENSION_E2E_DIR").map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/tilo-extension-e2e"),
            PathBuf::from,
        );
        fs::create_dir_all(&base).unwrap();
        let scratch = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&base)
            .unwrap();
        let dir = scratch.path().to_path_buf();
        let path = |name: &str| dir.join(name);
        for name in ["release", "rogue", "rekor"] {
            p256_key(&path(&format!("{name}.key")));
        }
        public_key(&path("release.key"), &path("release.pub"));
        public_key(&path("rekor.key"), &path("rekor.pub"));
        let ceremony = repo().join("tools/signing/release-ca.sh");
        let ceremony = ceremony.to_str().unwrap();
        for name in ["root", "other-root"] {
            let key = path(&format!("{name}.key"));
            run(
                "openssl",
                &[
                    "genpkey",
                    "-algorithm",
                    "EC",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-384",
                    "-out",
                    key.to_str().unwrap(),
                ],
            );
            run(
                ceremony,
                &[
                    "init-root",
                    "--key",
                    key.to_str().unwrap(),
                    "--out",
                    path(&format!("{name}.crt")).to_str().unwrap(),
                ],
            );
        }
        run(
            ceremony,
            &[
                "issue",
                "--root",
                path("root.crt").to_str().unwrap(),
                "--key",
                path("root.key").to_str().unwrap(),
                "--purpose",
                "extension-manifests",
                "--public-key",
                path("release.pub").to_str().unwrap(),
                "--days",
                "30",
                "--out",
                path("operational.crt").to_str().unwrap(),
            ],
        );
        Self {
            _scratch: scratch,
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// Publish the manifest pinning the fixture RPMs under `key`'s signature;
/// returns its URL.
fn publish(machine: &Machine, rpms: &BTreeMap<&str, PathBuf>, key: &Path) -> String {
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
            let path = &rpms[name];
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
    url
}

/// The cosign bundle beside the manifest at `url`: the release key's
/// signature, the operational certificate and a Rekor entry signed by `keys`'s
/// log key.
fn publish_bundle(machine: &Machine, keys: &Keys, url: &str) {
    let (data, signature) = {
        let web = machine.host.web.borrow();
        (web[url].clone(), web[&format!("{url}.sig")].clone())
    };
    let signature = String::from_utf8(signature).unwrap().trim().to_owned();
    let certificate = base64(&fs::read(keys.path("operational.crt")).unwrap());
    let body = json!({
        "apiVersion": "0.0.1", "kind": "hashedrekord",
        "spec": {"data": {"hash": {"algorithm": "sha256", "value": sha256_hex(&data)}},
                 "signature": {"content": signature, "publicKey": {"content": certificate}}},
    });
    let body = base64(&serde_json::to_vec(&body).unwrap());
    let der = run(
        "openssl",
        &[
            "pkey",
            "-pubin",
            "-in",
            keys.path("rekor.pub").to_str().unwrap(),
            "-outform",
            "DER",
        ],
    );
    let log_id = sha256_hex(&der);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let canonical = format!(
        "{{\"body\":\"{body}\",\"integratedTime\":{time},\"logID\":\"{log_id}\",\"logIndex\":1}}"
    );
    let timestamp = String::from_utf8(sign(
        canonical.as_bytes(),
        &keys.path("rekor.key"),
        &machine.root,
    ))
    .unwrap();
    let bundle = json!({
        "base64Signature": signature, "cert": certificate,
        "rekorBundle": {"SignedEntryTimestamp": timestamp,
                        "Payload": {"body": body, "integratedTime": time, "logIndex": 1,
                                    "logID": log_id}},
    });
    machine.host.web.borrow_mut().insert(
        format!("{url}.bundle"),
        serde_json::to_vec(&bundle).unwrap(),
    );
}

/// Make `machine` an image with the release root `root` at release epoch `epoch`.
fn certify(machine: &Machine, keys: &Keys, root: &str, epoch: u32) {
    let layout = &machine.layout;
    fs::create_dir_all(layout.release_root().parent().unwrap()).unwrap();
    fs::copy(keys.path(root), layout.release_root()).unwrap();
    fs::copy(keys.path("rekor.pub"), layout.rekor_key()).unwrap();
    let trust: String = fs::read_to_string(repo().join("keys/release/trust.env"))
        .unwrap()
        .lines()
        .map(|line| {
            if line.starts_with("TILO_RELEASE_EPOCH=") {
                format!("TILO_RELEASE_EPOCH={epoch}\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    fs::write(layout.release_trust(), trust).unwrap();
}

fn assert_refused(machine: &Machine, why: &str) {
    assert_eq!(machine.run("install"), 1, "{why}");
    let status = machine.status();
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("signature is not valid"),
        "{why}: {status}"
    );
    assert_eq!(status["state"], "failed", "{why}");
    assert!(!machine.codecs().exists(), "{why}");
}

/// Real packages install only under the trusted key's signature, or on an
/// image with the release root only under a certificate its root issued for
/// the current epoch; activation restores what was changed behind its back,
/// and removal leaves nothing.
#[test]
fn real_packages_install_only_under_the_signature_their_image_trusts() {
    let keys = Keys::new();
    let top = keys.path("rpmbuild");
    fs::create_dir_all(&top).unwrap();
    let rpms: BTreeMap<&str, PathBuf> = PACKAGES
        .iter()
        .map(|p| (p.name, build_rpm(&top, p)))
        .collect();
    let machine = Machine::with_host(Fake {
        real_tools: Some(tilo_test_env::sibling_binary("tilo-crypto", "tilo-crypto")),
        ..Fake::default()
    });
    fs::write(
        machine.root.join("usr/lib/os-release"),
        "ID=fedora\nVERSION_ID=44\n",
    )
    .unwrap();
    fs::copy(keys.path("release.pub"), machine.layout.trust_key()).unwrap();

    publish(&machine, &rpms, &keys.path("rogue.key"));
    assert_refused(&machine, "signed by another key");
    let url = publish(&machine, &rpms, &keys.path("release.key"));
    {
        let mut web = machine.host.web.borrow_mut();
        let text = String::from_utf8(web[&url].clone()).unwrap();
        web.insert(
            url,
            text.replacen(r#""license": "MIT""#, r#""license": "MIU""#, 1)
                .into_bytes(),
        );
    }
    assert_refused(&machine, "changed after signing");

    publish(&machine, &rpms, &keys.path("release.key"));
    assert_eq!(machine.run("install"), 0, "{}", machine.status());
    let status = machine.status();
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

    certify(&machine, &keys, "other-root.crt", 1);
    let url = publish(&machine, &rpms, &keys.path("release.key"));
    publish_bundle(&machine, &keys, &url);
    assert_refused(&machine, "a certificate from another root");
    certify(&machine, &keys, "root.crt", 2);
    assert_refused(&machine, "a certificate of a revoked epoch");
    certify(&machine, &keys, "root.crt", 1);
    assert_eq!(machine.run("install"), 0, "{}", machine.status());
    assert_eq!(machine.status()["state"], "installed");
    fs::remove_dir_all(machine.codecs()).unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert!(machine.codecs().is_dir());
}
