//! Language packs: one locale's catalogues, a signed package installed on
//! the path apps and runtimes take, into the person's language packs where
//! the runtime reads them. A new version replaces the locale's catalogues
//! whole, and a pack carries nothing but its locale's catalogues.

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};

fn manifest(version: &str, key: &SigningKey) -> Manifest {
    let identity = hex::encode(key.verifying_key().to_bytes());
    Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.language-pack.de-de"
version = "{version}"
kind = "language-pack"

[publisher]
identity = "ed25519:{identity}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#
    ))
    .unwrap()
}

/// A signed pack of `catalogues` (`(file, text)` under `de-DE/`).
fn pack_of(home: &Path, key: &SigningKey, version: &str, catalogues: &[(&str, &str)]) -> PathBuf {
    let payload = home.join(format!("payload-{version}"));
    for (file, text) in catalogues {
        let path = payload.join("de-DE").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let packed = pack(
        &payload,
        &manifest(version, key),
        &SigningIdentity::from_signing_key(key.clone()),
    )
    .expect("pack");
    let tapp = home.join(format!("de-de-{version}.tapp"));
    let file = std::fs::File::create(&tapp).unwrap();
    tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
    tapp
}

fn install(home: &Path, key: &SigningKey, tapp: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tilo-install"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .args([
            "install".as_ref(),
            tapp.as_os_str(),
            "--force".as_ref(),
            "--require-publisher".as_ref(),
            hex::encode(key.verifying_key().to_bytes()).as_ref(),
        ])
        .output()
        .expect("spawn tilo-install");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_language_pack_installs_where_the_runtime_reads_it_and_updates_whole() {
    let home = tempfile::tempdir().unwrap();
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let first = pack_of(
        home.path(),
        &key,
        "2026.10.1",
        &[
            ("settings-core.ftl", "settings-title = Einstellungen\n"),
            ("files.ftl", "files-title = Dateien\n"),
        ],
    );
    let (ok, out) = install(home.path(), &key, &first);
    assert!(ok, "{out}");
    let installed = home.path().join("data/tilo/language-packs/de-DE");
    assert_eq!(
        std::fs::read_to_string(installed.join("settings-core.ftl")).unwrap(),
        "settings-title = Einstellungen\n"
    );
    assert_eq!(
        std::fs::read_to_string(installed.join(".package")).unwrap(),
        "so.tilo.language-pack.de-de 2026.10.1\n"
    );

    // The next version replaces the catalogues whole: one it no longer
    // carries is gone.
    let second = pack_of(
        home.path(),
        &key,
        "2026.10.2",
        &[(
            "settings-core.ftl",
            "settings-title = Systemeinstellungen\n",
        )],
    );
    let (ok, out) = install(home.path(), &key, &second);
    assert!(ok, "{out}");
    assert_eq!(
        std::fs::read_to_string(installed.join("settings-core.ftl")).unwrap(),
        "settings-title = Systemeinstellungen\n"
    );
    assert!(!installed.join("files.ftl").exists());
    let leftovers: Vec<_> = std::fs::read_dir(home.path().join("data/tilo/language-packs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["de-DE"], "no staging is left behind");

    // A publisher the install does not require is refused, and nothing changes.
    let stranger = SigningKey::generate(&mut UnwrapErr(SysRng));
    let forged = pack_of(
        home.path(),
        &stranger,
        "2026.10.3",
        &[("settings-core.ftl", "x = y\n")],
    );
    assert!(!install(home.path(), &key, &forged).0);
    assert_eq!(
        std::fs::read_to_string(installed.join("settings-core.ftl")).unwrap(),
        "settings-title = Systemeinstellungen\n"
    );
}

/// A pack is its locale's catalogues and nothing else: one carrying another
/// locale or a program does not pack.
#[test]
fn a_language_pack_carries_only_its_locales_catalogues() {
    let home = tempfile::tempdir().unwrap();
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    for (directory, file) in [("fr-FR", "settings-core.ftl"), ("de-DE", "bin/tool")] {
        let payload = home.path().join(format!("bad-{directory}"));
        let path = payload.join(directory).join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
        assert!(
            pack(
                &payload,
                &manifest("2026.10.1", &key),
                &SigningIdentity::from_signing_key(key.clone())
            )
            .is_err(),
            "{directory}/{file}"
        );
    }
}
