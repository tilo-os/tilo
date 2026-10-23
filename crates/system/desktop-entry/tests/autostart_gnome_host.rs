//! The autostart entries of a Fedora/Bazzite GNOME laptop running a Tilo
//! session. The fixture files are that machine's `/etc/xdg/autostart` and
//! `~/.config/autostart`, translations stripped and user paths anonymised.
//! Every one of them used to start at login, including the screen reader,
//! GNOME's keyring and settings-daemon plugins and a second tray watcher.

use std::path::{Path, PathBuf};

use desktop_entry::autostart::{AutostartProbe, Skipped, scan_autostart};

/// The laptop: programs on `PATH`, the screen reader off, toolkit
/// accessibility on (as Bazzite ships it).
struct Laptop;

impl AutostartProbe for Laptop {
    fn program_exists(&self, program: &str) -> bool {
        ["xrefresh", "xdg-user-dirs-update"].contains(&program)
    }
    fn gsettings_bool(&self, schema: &str, key: &str) -> Option<bool> {
        match (schema, key) {
            ("org.gnome.desktop.a11y.applications", "screen-reader-enabled") => Some(false),
            ("org.gnome.desktop.interface", "toolkit-accessibility") => Some(true),
            _ => None,
        }
    }
    fn config_file_exists(&self, _: &str) -> bool {
        false
    }
}

fn fixture() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/autostart-gnome-host");
    vec![
        root.join("home/.config/autostart"),
        root.join("etc/xdg/autostart"),
    ]
}

#[test]
fn only_what_gnome_would_start_outside_its_own_session_starts() {
    let scan = scan_autostart(&fixture(), &Laptop);
    let started: Vec<&str> = scan
        .start
        .iter()
        .map(|start| start.entry.id.as_str())
        .collect();
    assert_eq!(
        started,
        vec![
            "Quadrant",
            "example-appimage",
            // Desktop-neutral entries every desktop starts. The guest-tool
            // wrappers decide for themselves and exit outside a VM.
            "bazzite-announcement",
            "geoclue-demo-agent",
            "input-remapper-autoload",
            "nvidia-settings-load",
            "org.gnome.Evolution-alarm-notify",
            "user-dirs-update-gtk",
            "vboxclient",
            "vmware-user",
            "xdg-user-dirs",
        ]
    );
    assert!(scan.start.iter().all(|start| !start.auto_restart));

    let reason = |id: &str| {
        scan.skipped
            .iter()
            .find(|(path, _)| path.file_stem().is_some_and(|stem| stem == id))
            .map(|(_, reason)| reason.clone())
    };
    for gnome_session_part in [
        "at-spi-dbus-bus",
        "gnome-keyring-pkcs11",
        "gnome-keyring-secrets",
        "gnome-keyring-ssh",
        "ibus-mozc-launch-xwayland",
        "localsearch-3",
        "orca-autostart",
        "org.gnome.SettingsDaemon.DiskUtilityNotify",
    ] {
        assert_eq!(
            reason(gnome_session_part),
            Some(Skipped::NotShownIn),
            "{gnome_session_part}"
        );
    }
    assert_eq!(
        reason("spice-vdagent"),
        Some(Skipped::DesktopPhase("WindowManager".into()))
    );
    assert_eq!(
        reason("xapp-sn-watcher"),
        Some(Skipped::DesktopPhase("Panel".into()))
    );
    assert_eq!(scan.start.len() + scan.skipped.len(), 21);
}
