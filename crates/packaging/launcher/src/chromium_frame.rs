//! Chromium-family host browsers use the system title bar under Tilo.
//!
//! The OS owns window chrome: every window already gets Tilo's window bar,
//! and the compositor answers every xdg-decoration request with
//! server-side decorations. Chromium ignores that answer while its
//! profile asks for its own custom frame, which is the default on Wayland,
//! and draws a second title bar with its own window buttons under Tilo's.
//! No command-line switch changes this; the one lever is the profile
//! preference behind "Use system title bar and borders",
//! `browser.custom_chrome_frame`. With it `false` Chromium honours the
//! server-side answer, and only its tab strip, which is content, remains.
//!
//! So before a Chromium-family host app starts, its default profile gets
//! that preference when it has none: a new profile is seeded with it, and
//! an existing one has exactly that key merged in, every other key and
//! value kept. A value already there is the person's choice and stays.
//! A profile that is unreadable, not JSON or shaped unexpectedly is left
//! untouched, a running browser's profile is left to the browser, and a
//! launch naming its own `--user-data-dir` is not the default profile and
//! is not touched.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use desktop_entry::EntrySource;
use desktop_entry::launch::HostLaunch;
use serde_json::{Map, Value};

/// The section and key of Chromium's "Use system title bar and borders".
const SECTION: &str = "browser";
const CUSTOM_FRAME: &str = "custom_chrome_frame";

/// A browser's user-data directory, relative to the configuration home it
/// runs with, and the profile the launch opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromiumProfile {
    pub user_data_dir: PathBuf,
    pub profile: String,
}

impl ChromiumProfile {
    fn preferences(&self) -> PathBuf {
        self.user_data_dir.join(&self.profile).join("Preferences")
    }
}

/// What preparing a profile did, for the launcher's log and for tests.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A new profile, seeded with the system title bar.
    Seeded,
    /// An existing profile, the key merged in beside everything it had.
    Merged,
    /// The profile already states a frame preference; it stays.
    AlreadySet,
    /// The browser is running and owns its profile; nothing written.
    Running,
    /// The file is not a JSON object of the expected shape; left untouched.
    Unreadable,
}

/// The default profile of the Chromium-family browser `launch` starts, or
/// `None` when it is no such browser or names a user-data directory of
/// its own.
pub fn profile_for(launch: &HostLaunch) -> Option<ChromiumProfile> {
    if launch
        .args
        .iter()
        .any(|arg| arg == "--user-data-dir" || arg.starts_with("--user-data-dir="))
    {
        return None;
    }
    let home = PathBuf::from(launch.env.get("HOME")?);
    let user_data_dir = match &launch.source {
        EntrySource::Flatpak { app_id } => {
            // A Flatpak's configuration home is its own, whatever the host's is.
            let relative = flatpak_user_data_dir(app_id)?;
            home.join(".var/app")
                .join(app_id)
                .join("config")
                .join(relative)
        }
        EntrySource::Host => {
            let program = launch.program.file_name()?.to_str()?;
            let relative = program_user_data_dir(program)?;
            let config_home = launch
                .env
                .get("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .unwrap_or_else(|| home.join(".config"));
            config_home.join(relative)
        }
    };
    Some(ChromiumProfile {
        user_data_dir,
        profile: profile_directory(&launch.args)?,
    })
}

fn program_user_data_dir(program: &str) -> Option<&'static str> {
    Some(match program {
        "chromium" | "chromium-browser" | "chromium-freeworld" => "chromium",
        "google-chrome" | "google-chrome-stable" => "google-chrome",
        "google-chrome-beta" => "google-chrome-beta",
        "google-chrome-unstable" => "google-chrome-unstable",
        "brave" | "brave-browser" | "brave-browser-stable" => "BraveSoftware/Brave-Browser",
        "microsoft-edge" | "microsoft-edge-stable" => "microsoft-edge",
        "microsoft-edge-beta" => "microsoft-edge-beta",
        "microsoft-edge-dev" => "microsoft-edge-dev",
        _ => return None,
    })
}

fn flatpak_user_data_dir(app_id: &str) -> Option<&'static str> {
    Some(match app_id {
        "org.chromium.Chromium" | "io.github.ungoogled_software.ungoogled_chromium" => "chromium",
        "com.google.Chrome" => "google-chrome",
        "com.google.ChromeDev" => "google-chrome-unstable",
        "com.brave.Browser" => "BraveSoftware/Brave-Browser",
        "com.microsoft.Edge" => "microsoft-edge",
        _ => return None,
    })
}

/// `Default`, or the launch's `--profile-directory` when it is one plain
/// directory name. Anything else is not a profile this module can name
/// safely, and `None` leaves the launch alone.
fn profile_directory(args: &[String]) -> Option<String> {
    let mut named = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--profile-directory=") {
            named = Some(value.to_string());
        } else if arg == "--profile-directory" {
            named = Some(args.next()?.clone());
        }
    }
    let Some(named) = named else {
        return Some("Default".to_string());
    };
    let mut components = Path::new(&named).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Some(named),
        _ => None,
    }
}

/// Give `profile` the system title bar unless it already states a frame
/// preference, preserving everything else in it.
///
/// # Errors
///
/// Creating the profile directory or writing the file failed. A file that
/// can't be parsed is not an error: it is left as it is.
pub fn prefer_system_frame(profile: &ChromiumProfile) -> std::io::Result<Outcome> {
    // A running browser holds its profile in memory and writes it back on
    // exit; it removes this link when it ends.
    if profile
        .user_data_dir
        .join("SingletonLock")
        .symlink_metadata()
        .is_ok()
    {
        return Ok(Outcome::Running);
    }
    let path = profile.preferences();
    let (mut document, outcome) = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(document)) => (document, Outcome::Merged),
            _ => return Ok(Outcome::Unreadable),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Map::new(), Outcome::Seeded),
        Err(_) => return Ok(Outcome::Unreadable),
    };
    let section = document
        .entry(SECTION)
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(section) = section else {
        return Ok(Outcome::Unreadable);
    };
    if section.contains_key(CUSTOM_FRAME) {
        return Ok(Outcome::AlreadySet);
    }
    section.insert(CUSTOM_FRAME.to_string(), Value::Bool(false));
    let bytes = serde_json::to_vec(&Value::Object(document)).map_err(std::io::Error::other)?;
    write_atomically(&path, &bytes)?;
    Ok(outcome)
}

/// Replace `path` with `bytes` in one rename, so a crash leaves either the
/// old file or the new one, never half of either. Private to the person,
/// as Chromium creates its profile.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the profile file has no directory"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let mut file = tempfile::Builder::new()
        .prefix(".Preferences.tilo-")
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(dir)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    std::fs::File::open(dir)?.sync_all()
}

/// [`prefer_system_frame`] for the browser `launch` starts, if it is one.
/// Never fails the launch: the browser starts either way, with the frame
/// it would have had.
pub fn prepare(launch: &HostLaunch) {
    let Some(profile) = profile_for(launch) else {
        return;
    };
    let file = profile.preferences();
    match prefer_system_frame(&profile) {
        Ok(Outcome::Seeded | Outcome::Merged) => eprintln!(
            "tilo-launcher: {} uses the system title bar ({})",
            launch.desktop_id,
            file.display()
        ),
        Ok(Outcome::Unreadable) => eprintln!(
            "tilo-launcher: {} keeps its own title bar: {} is not readable JSON and is left as it is",
            launch.desktop_id,
            file.display()
        ),
        Ok(Outcome::AlreadySet | Outcome::Running) => {}
        Err(error) => eprintln!(
            "tilo-launcher: {} keeps its own title bar: writing {} failed: {error}",
            launch.desktop_id,
            file.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt as _;

    fn launch(program: &str, source: EntrySource, args: &[&str], home: &Path) -> HostLaunch {
        HostLaunch {
            desktop_id: "browser.desktop".into(),
            display_name: "Browser".into(),
            program: PathBuf::from(program),
            args: args.iter().map(ToString::to_string).collect(),
            env: BTreeMap::from([("HOME".to_string(), home.display().to_string())]),
            window_identities: Vec::new(),
            source,
        }
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_new_chromium_profile_is_seeded_with_the_system_title_bar() {
        let home = tempfile::tempdir().unwrap();
        let profile = profile_for(&launch(
            "/usr/bin/chromium-browser",
            EntrySource::Host,
            &["--new-window"],
            home.path(),
        ))
        .unwrap();
        assert_eq!(profile.user_data_dir, home.path().join(".config/chromium"));

        assert_eq!(prefer_system_frame(&profile).unwrap(), Outcome::Seeded);
        let preferences = home.path().join(".config/chromium/Default/Preferences");
        assert_eq!(
            read(&preferences),
            serde_json::json!({ "browser": { "custom_chrome_frame": false } })
        );
        let mode = std::fs::metadata(&preferences)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn an_existing_profile_gets_only_the_frame_key_and_keeps_every_other_setting() {
        let home = tempfile::tempdir().unwrap();
        let dir = home
            .path()
            .join(".var/app/com.google.Chrome/config/google-chrome/Work");
        std::fs::create_dir_all(&dir).unwrap();
        let before = serde_json::json!({
            "browser": { "has_seen_welcome_page": true, "window_placement": { "maximized": true } },
            "homepage": "https://example.org/",
            "profile": { "name": "Work", "exit_type": "Normal", "content_settings": { "zoom": 1.25 } },
            "extensions": { "pinned_extensions": ["abc", "def"] },
            "counter": 13_284_123_456_789_u64
        });
        std::fs::write(
            dir.join("Preferences"),
            serde_json::to_vec(&before).unwrap(),
        )
        .unwrap();

        let profile = profile_for(&launch(
            "/usr/bin/flatpak",
            EntrySource::Flatpak {
                app_id: "com.google.Chrome".into(),
            },
            &["run", "com.google.Chrome", "--profile-directory=Work"],
            home.path(),
        ))
        .unwrap();
        assert_eq!(prefer_system_frame(&profile).unwrap(), Outcome::Merged);

        let mut expected = before;
        expected["browser"]["custom_chrome_frame"] = Value::Bool(false);
        assert_eq!(read(&dir.join("Preferences")), expected);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "Preferences")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_frame_the_person_chose_stays() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".config/chromium/Default");
        std::fs::create_dir_all(&dir).unwrap();
        let chosen = br#"{"browser":{"custom_chrome_frame":true},"homepage":"x"}"#;
        std::fs::write(dir.join("Preferences"), chosen).unwrap();
        let profile = ChromiumProfile {
            user_data_dir: home.path().join(".config/chromium"),
            profile: "Default".into(),
        };
        assert_eq!(prefer_system_frame(&profile).unwrap(), Outcome::AlreadySet);
        assert_eq!(std::fs::read(dir.join("Preferences")).unwrap(), chosen);
    }

    #[test]
    fn a_corrupt_or_unexpected_profile_is_left_untouched() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".config/chromium/Default");
        std::fs::create_dir_all(&dir).unwrap();
        let profile = ChromiumProfile {
            user_data_dir: home.path().join(".config/chromium"),
            profile: "Default".into(),
        };
        for contents in [
            &b"{\"browser\": {\"custom_chrome_fr"[..],
            b"",
            b"[1, 2, 3]",
            b"{\"browser\": \"not an object\"}",
            &[0xff, 0xfe, 0x00][..],
        ] {
            std::fs::write(dir.join("Preferences"), contents).unwrap();
            assert_eq!(prefer_system_frame(&profile).unwrap(), Outcome::Unreadable);
            assert_eq!(std::fs::read(dir.join("Preferences")).unwrap(), contents);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        }
    }

    #[test]
    fn a_running_browsers_profile_is_left_to_the_browser() {
        let home = tempfile::tempdir().unwrap();
        let user_data_dir = home.path().join(".config/chromium");
        std::fs::create_dir_all(&user_data_dir).unwrap();
        std::os::unix::fs::symlink("host-4242", user_data_dir.join("SingletonLock")).unwrap();
        let profile = ChromiumProfile {
            user_data_dir: user_data_dir.clone(),
            profile: "Default".into(),
        };
        assert_eq!(prefer_system_frame(&profile).unwrap(), Outcome::Running);
        assert!(!user_data_dir.join("Default").exists());
    }

    #[test]
    fn only_default_profiles_of_chromium_family_browsers_are_touched() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert_eq!(
            profile_for(&launch("/usr/bin/firefox", EntrySource::Host, &[], home)),
            None
        );
        assert_eq!(
            profile_for(&launch(
                "/usr/bin/flatpak",
                EntrySource::Flatpak {
                    app_id: "org.mozilla.firefox".into()
                },
                &[],
                home
            )),
            None
        );
        for args in [
            &["--user-data-dir=/srv/kiosk"][..],
            &["--user-data-dir", "/srv/kiosk"],
            &["--profile-directory=../elsewhere"],
            &["--profile-directory", "/abs"],
        ] {
            assert_eq!(
                profile_for(&launch("/usr/bin/chromium", EntrySource::Host, args, home)),
                None,
                "{args:?}"
            );
        }
        let mut with_config_home = launch("/usr/bin/brave-browser", EntrySource::Host, &[], home);
        with_config_home
            .env
            .insert("XDG_CONFIG_HOME".into(), "/elsewhere/config".into());
        assert_eq!(
            profile_for(&with_config_home),
            Some(ChromiumProfile {
                user_data_dir: PathBuf::from("/elsewhere/config/BraveSoftware/Brave-Browser"),
                profile: "Default".into(),
            })
        );
    }
}
