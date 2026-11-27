//! `app repo`: the repositories a device reads, who may change them, and what a bad change is refused.

use std::path::Path;
use std::process::{Command, Output};
use tilo_test_env::SpawnUnderEnvLock as _;

use app_cli::RegistriesFile;

use crate::daemon_fixture::Daemon;
use crate::registry_fixture::Device;

const OFFICIAL_KEY: &str = "d5dd5851bebac876b19986485184213ead36b67595b7efedf1144c120033b0bb";
const MIRROR_KEY: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

fn app(args: &[&str]) -> Output {
    Command::new(tilo_test_env::package_binary!("app"))
        .args(args)
        .output_locked()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn repositories(_device: &Device) -> RegistriesFile {
    let out = app(&["repo", "list", "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    serde_json::from_slice(&out.stdout).expect("the repository list is JSON")
}

/// A trust anchor file naming `OFFICIAL_KEY`, as an image ships it.
fn trust_anchor(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("registry-owner.pub");
    std::fs::write(&path, format!("{OFFICIAL_KEY}\n")).unwrap();
    path
}

fn device_with_anchor() -> (Device, tempfile::TempDir) {
    let anchor = tempfile::tempdir().unwrap();
    let trust = trust_anchor(anchor.path());
    let device = Device::new(None, &[("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH", &trust)]);
    (device, anchor)
}

tilo_test_env::scenario! {
    fn repositories_are_seeded_validated_and_kept_editable;

    phase the_first_read_seeds_the_official_repository_and_keeps_its_pin;
    {
        let (device, _anchor) = device_with_anchor();
        let _daemon = Daemon::start(device.apps.path());
        let seeded = repositories(&device);
        assert_eq!(seeded.registries.len(), 1);
        assert_eq!(seeded.registries[0].name, "Tilo main");
        assert_eq!(seeded.registries[0].base_url, "https://apps.tilo.so");
        assert_eq!(seeded.registries[0].owner_pubkey_hex, OFFICIAL_KEY);
        assert!(seeded.registries[0].enabled);
        assert_eq!(
            seeded.registries[0].channel,
            tilo_app_package::version::ReleaseChannel::Stable,
            "a fresh device follows stable releases unless told otherwise"
        );
        assert!(device.config.path().join("registries.toml").exists());

        // A hand edit that emptied the official pin is repaired on the next
        // read, so a stale key cannot quietly disable trust in the hosted
        // registry.
        let placeholder = "0000000000000000000000000000000000000000000000000000000000000000";
        std::fs::write(
            device.config.path().join("registries.toml"),
            format!(
                "[[registry]]\nname = \"Tilo main\"\nbase_url = \"https://apps.tilo.so\"\n\
                 owner_pubkey_hex = \"{placeholder}\"\n"
            ),
        )
        .unwrap();
        assert_eq!(
            repositories(&device).registries[0].owner_pubkey_hex,
            OFFICIAL_KEY
        );
    }

    phase repository_changes_validate_and_never_overwrite_a_broken_file;
    {
        let (device, _anchor) = device_with_anchor();
        let _daemon = Daemon::start(device.apps.path());
        let path = device.config.path().join("registries.toml");
        repositories(&device);
        let seeded = std::fs::read_to_string(&path).unwrap();
        let placeholder = "00".repeat(32);
        for bad in [
            vec!["repo", "add", "", "https://mirror.example", "--owner-key", MIRROR_KEY],
            vec!["repo", "add", "mirror", "file:///tmp/apps", "--owner-key", MIRROR_KEY],
            vec!["repo", "add", "mirror", "http://mirror.example", "--owner-key", MIRROR_KEY],
            vec![
                "repo",
                "add",
                "mirror",
                "https://user:secret@mirror.example",
                "--owner-key",
                MIRROR_KEY,
            ],
            vec![
                "repo",
                "add",
                "mirror",
                "https://mirror.example?next=https://evil.example",
                "--owner-key",
                MIRROR_KEY,
            ],
            vec!["repo", "add", "mirror", "https://mirror.example", "--owner-key", "bad"],
            vec![
                "repo",
                "add",
                "mirror",
                "https://mirror.example",
                "--owner-key",
                placeholder.as_str(),
            ],
            vec!["repo", "add", "Tilo main", "https://mirror.example", "--owner-key", MIRROR_KEY],
        ] {
            let refused = app(&bad);
            assert!(!refused.status.success(), "{bad:?} was accepted");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                seeded,
                "{bad:?} changed the file it refused"
            );
        }

        let added = app(&[
            "repo",
            "add",
            "mirror",
            "https://mirror.example/",
            "--owner-key",
            MIRROR_KEY,
        ]);
        assert!(added.status.success(), "{}", text(&added));
        let registries = repositories(&device);
        assert_eq!(registries.registries.len(), 2);
        assert_eq!(registries.registries[1].name, "mirror");
        assert_eq!(
            registries.registries[1].base_url, "https://mirror.example",
            "a trailing slash is normalised"
        );
        assert!(registries.registries[1].enabled);

        assert!(app(&["repo", "disable", "mirror"]).status.success());
        assert!(!repositories(&device).registries[1].enabled);
        assert!(app(&["repo", "enable", "mirror"]).status.success());
        assert!(repositories(&device).registries[1].enabled);

        assert!(app(&["repo", "remove", "Tilo main"]).status.success());
        let last = app(&["repo", "remove", "mirror"]);
        assert!(!last.status.success(), "the last repository cannot be removed");
        assert!(
            text(&last).contains("disable it instead"),
            "the refusal says what to do: {}",
            text(&last)
        );
        assert_eq!(repositories(&device).registries.len(), 1);

        // The person's broken file is theirs to fix: a read answers with the
        // seed in memory, and a change refuses rather than overwriting it.
        let broken =
            "[[registry]]\nname = \"My mirror\"\nbase_url = \"https://mirror.example\n";
        std::fs::write(&path, broken).unwrap();
        let listed = app(&["repo", "list", "--json"]);
        assert!(listed.status.success(), "{}", text(&listed));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        assert!(
            !app(&[
                "repo",
                "add",
                "another",
                "https://other.example",
                "--owner-key",
                MIRROR_KEY,
            ])
            .status
            .success()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    phase a_channel_is_selected_explicitly_and_persisted;
    {
        let (device, _anchor) = device_with_anchor();
        let _daemon = Daemon::start(device.apps.path());
        let path = device.config.path().join("registries.toml");
        std::fs::write(
            &path,
            format!(
                "[[registry]]\nname = \"Tilo main\"\nbase_url = \"https://apps.tilo.so\"\n\
                 owner_pubkey_hex = \"{OFFICIAL_KEY}\"\n"
            ),
        )
        .unwrap();
        assert_eq!(
            repositories(&device).registries[0].channel,
            tilo_app_package::version::ReleaseChannel::Stable,
            "a configuration that names no channel follows stable releases"
        );
        for (channel, expected) in [
            ("development", tilo_app_package::version::ReleaseChannel::Development),
            ("stable", tilo_app_package::version::ReleaseChannel::Stable),
        ] {
            assert!(app(&["repo", "channel", "Tilo main", channel]).status.success());
            assert_eq!(repositories(&device).registries[0].channel, expected);
        }
        let before = std::fs::read(&path).unwrap();
        assert!(!app(&["repo", "channel", "Tilo main", "typo"]).status.success());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    phase a_device_with_no_trust_file_seeds_the_compiled_main_registry;
    {
        let anchor = tempfile::tempdir().unwrap();
        let device = Device::new(
            None,
            &[(
                "TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH",
                &anchor.path().join("missing.pub"),
            )],
        );
        let _daemon = Daemon::start(device.apps.path());
        let seeded = repositories(&device);
        assert_eq!(seeded.registries.len(), 1);
        assert_eq!(seeded.registries[0].name, "Tilo main");
        assert_eq!(seeded.registries[0].base_url, "https://apps.tilo.so");
        assert_eq!(
            seeded.registries[0].owner_pubkey_hex,
            app_cli::config::MAIN_REGISTRY_OWNER_PUBKEY_HEX,
            "an image with no key file trusts the compiled root"
        );
        assert!(
            device.config.path().join("registries.toml").exists(),
            "the seeded registry is the record the workspace keeps"
        );
    }

    phase a_reader_that_must_not_wait_writes_nothing;
    {
        // The desktop's search reads the kept catalogues synchronously and
        // must not create configuration as a side effect of being drawn.
        let (device, _anchor) = device_with_anchor();
        let _daemon = Daemon::start(device.apps.path());
        assert!(app_cli::last_accepted_offers().is_empty());
        assert!(
            !device.config.path().join("registries.toml").exists(),
            "the no-seed reader must leave the config dir untouched"
        );
    }
}
