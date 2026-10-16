//! Chromium-family browsers start with the system title bar.
//!
//! Tilo owns window chrome: every window sits in a group under Tilo's tab
//! row. On Wayland, Chromium defaults to its own client-side frame and asks
//! for client-side decoration (`zxdg_toplevel_decoration_v1.set_mode(1)`).
//! Tilo answers server-side, but the custom frame does not depend on that
//! answer: Chromium keeps drawing its caption buttons and drag area beside
//! its tab strip, a second bar under Tilo's tab row. Chromium has no
//! protocol hint, switch or policy for this; the choice is the profile
//! preference behind "Use system title bar and borders",
//! `browser.custom_chrome_frame`, whose Wayland default is `true`. With it
//! `false` Chromium asks for server-side decoration and draws its tab strip
//! with no caption buttons, so Tilo's tab row is the only window chrome.
//!
//! So a host launch of a Chromium-family browser seeds that preference to
//! `false` in each of the browser's profiles that has not set it, once per
//! profile: a person who later turns the custom frame back on in Chromium's
//! settings keeps it, because Chromium stores a preference equal to its
//! default by leaving it out, which a second seeding would undo. A browser
//! that is running holds its preferences in memory and writes them back on
//! exit, so its profiles are left for a later launch. Seeding is best effort:
//! nothing here can make a launch fail.

use std::io;
use std::path::{Path, PathBuf};

use desktop_entry::EntrySource;
use desktop_entry::launch::HostLaunch;

/// Where a Chromium-family browser keeps its profiles, below the
/// configuration directory (`$XDG_CONFIG_HOME`, or a Flatpak's own
/// `~/.var/app/<id>/config`).
const SYSTEM_BROWSERS: &[(&str, &str)] = &[
    ("chromium-browser", "chromium"),
    ("chromium", "chromium"),
    ("google-chrome", "google-chrome"),
    ("google-chrome-stable", "google-chrome"),
    ("google-chrome-beta", "google-chrome-beta"),
    ("google-chrome-unstable", "google-chrome-unstable"),
    ("brave-browser", "BraveSoftware/Brave-Browser"),
    ("microsoft-edge", "microsoft-edge"),
    ("microsoft-edge-stable", "microsoft-edge"),
    ("vivaldi-stable", "vivaldi"),
];

/// Flatpak application ids and the profile directory inside their config.
const FLATPAK_BROWSERS: &[(&str, &str)] = &[
    ("org.chromium.Chromium", "chromium"),
    (
        "io.github.ungoogled_software.ungoogled_chromium",
        "chromium",
    ),
    ("com.google.Chrome", "google-chrome"),
    ("com.google.ChromeDev", "google-chrome-unstable"),
    ("com.brave.Browser", "BraveSoftware/Brave-Browser"),
    ("com.microsoft.Edge", "microsoft-edge"),
    ("com.vivaldi.Vivaldi", "vivaldi"),
];

/// The preference file of one Chromium profile, below the user-data dir.
const PREFERENCES: &str = "Preferences";
/// The profile a fresh user-data dir starts with.
const DEFAULT_PROFILE: &str = "Default";
/// Present while a browser holds this user-data dir.
const SINGLETON_LOCK: &str = "SingletonLock";
/// The profiles already seeded, one absolute path per line.
const SEEDED_LIST: &str = "tilo/chromium-system-title-bar";

/// The user-data dir `launch` runs a Chromium-family browser with, or `None`
/// for anything else.
#[must_use]
pub fn user_data_dir(launch: &HostLaunch) -> Option<PathBuf> {
    let home = launch.env.get("HOME").map(PathBuf::from)?;
    let id = launch.desktop_id.trim_end_matches(".desktop");
    if let EntrySource::Flatpak { app_id } = &launch.source {
        // `--user-data-dir` would name a path inside the sandbox.
        let (_, dir) = FLATPAK_BROWSERS.iter().find(|(app, _)| app == app_id)?;
        return Some(home.join(".var/app").join(app_id).join("config").join(dir));
    }
    let program = launch
        .program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (_, dir) = SYSTEM_BROWSERS
        .iter()
        .find(|(name, _)| *name == id || *name == program)?;
    if let Some(dir) = explicit_user_data_dir(&launch.args) {
        return Some(dir);
    }
    let config = launch
        .env
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    Some(config.join(dir))
}

/// `--user-data-dir=DIR` or `--user-data-dir DIR` on the command line.
fn explicit_user_data_dir(args: &[String]) -> Option<PathBuf> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(dir) = arg.strip_prefix("--user-data-dir=") {
            return Some(PathBuf::from(dir)).filter(|dir| dir.is_absolute());
        }
        if arg == "--user-data-dir" {
            return args
                .next()
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute());
        }
    }
    None
}

/// Seed the system title bar into the profiles of the browser `launch`
/// starts. Reports what it could not do; the caller logs it and launches
/// regardless.
///
/// # Errors
///
/// Returns the first I/O error met reading or writing a profile.
pub fn seed(launch: &HostLaunch) -> io::Result<Vec<PathBuf>> {
    let Some(user_data) = user_data_dir(launch) else {
        return Ok(Vec::new());
    };
    let Some(state) = state_dir(launch) else {
        return Ok(Vec::new());
    };
    seed_user_data_dir(&user_data, &state.join(SEEDED_LIST))
}

fn state_dir(launch: &HostLaunch) -> Option<PathBuf> {
    launch
        .env
        .get("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            launch
                .env
                .get("HOME")
                .map(|home| Path::new(home).join(".local/state"))
        })
}

/// Seed every profile below `user_data` not yet listed in `seeded`, and
/// list it. A browser that has never run gets its first profile seeded
/// before it starts. Returns the profiles it set.
///
/// # Errors
///
/// Returns the first I/O error met reading or writing a profile or the list.
pub fn seed_user_data_dir(user_data: &Path, seeded: &Path) -> io::Result<Vec<PathBuf>> {
    // A running browser rewrites its preferences from memory on exit.
    if user_data.join(SINGLETON_LOCK).symlink_metadata().is_ok() {
        return Ok(Vec::new());
    }
    let done = match std::fs::read_to_string(seeded) {
        Ok(list) => list.lines().map(PathBuf::from).collect::<Vec<_>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut set = Vec::new();
    for profile in profiles(user_data)? {
        if done.contains(&profile) {
            continue;
        }
        seed_profile(&profile)?;
        set.push(profile);
    }
    if !set.is_empty() {
        if let Some(parent) = seeded.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut list = done;
        list.extend(set.iter().cloned());
        let mut text = String::new();
        for path in &list {
            text.push_str(&path.to_string_lossy());
            text.push('\n');
        }
        replace(seeded, text.as_bytes())?;
    }
    Ok(set)
}

/// The profile directories of `user_data`: every directory holding a
/// preference file, or the default profile of one that holds none yet.
fn profiles(user_data: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    match std::fs::read_dir(user_data) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.join(PREFERENCES).is_file() {
                    found.push(path);
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if found.is_empty() {
        found.push(user_data.join(DEFAULT_PROFILE));
    }
    found.sort();
    Ok(found)
}

/// Set `browser.custom_chrome_frame` to `false` in `profile`'s preferences
/// unless they already say either way. Everything else is kept as it was.
fn seed_profile(profile: &Path) -> io::Result<()> {
    let path = profile.join(PREFERENCES);
    let mut prefs = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error),
    };
    let Some(root) = prefs.as_object_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Chromium preferences are not an object",
        ));
    };
    let browser = root
        .entry("browser")
        .or_insert_with(|| serde_json::json!({}));
    let Some(browser) = browser.as_object_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Chromium's browser preferences are not an object",
        ));
    };
    if browser.contains_key("custom_chrome_frame") {
        return Ok(());
    }
    browser.insert(
        "custom_chrome_frame".to_owned(),
        serde_json::Value::Bool(false),
    );
    std::fs::create_dir_all(profile)?;
    let bytes = serde_json::to_vec(&prefs).map_err(io::Error::other)?;
    replace(&path, &bytes)
}

/// Replace `path` with `bytes` in one rename, private to this user as
/// Chromium keeps its preferences, so a browser starting meanwhile reads the
/// old file or the new one, never half of one.
fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map(|_| ()).map_err(|error| error.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn launch(desktop_id: &str, program: &str, source: EntrySource, home: &Path) -> HostLaunch {
        HostLaunch {
            desktop_id: desktop_id.to_owned(),
            display_name: desktop_id.to_owned(),
            program: PathBuf::from(program),
            args: Vec::new(),
            env: BTreeMap::from([("HOME".to_owned(), home.display().to_string())]),
            window_identities: Vec::new(),
            source,
        }
    }

    fn frame_pref(profile: &Path) -> Option<serde_json::Value> {
        let prefs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(profile.join(PREFERENCES)).ok()?).ok()?;
        prefs.get("browser")?.get("custom_chrome_frame").cloned()
    }

    #[test]
    fn chromium_family_browsers_are_found_where_they_keep_profiles() {
        let home = Path::new("/home/someone");
        let system = launch(
            "chromium-browser",
            "/usr/bin/chromium-browser",
            EntrySource::Host,
            home,
        );
        assert_eq!(user_data_dir(&system), Some(home.join(".config/chromium")));
        let flatpak = launch(
            "io.github.ungoogled_software.ungoogled_chromium",
            "/usr/bin/flatpak",
            EntrySource::Flatpak {
                app_id: "io.github.ungoogled_software.ungoogled_chromium".to_owned(),
            },
            home,
        );
        assert_eq!(
            user_data_dir(&flatpak),
            Some(
                home.join(
                    ".var/app/io.github.ungoogled_software.ungoogled_chromium/config/chromium"
                )
            )
        );
        let mut explicit = system;
        explicit.args = vec!["--user-data-dir=/tmp/elsewhere".to_owned()];
        assert_eq!(
            user_data_dir(&explicit),
            Some(PathBuf::from("/tmp/elsewhere"))
        );
        let firefox = launch(
            "org.mozilla.firefox",
            "/usr/bin/firefox",
            EntrySource::Host,
            home,
        );
        assert_eq!(user_data_dir(&firefox), None, "not a Chromium browser");
    }

    #[test]
    fn a_profile_gets_the_system_title_bar_once_and_keeps_its_other_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user_data = dir.path().join("chromium");
        let seeded = dir.path().join("state/seeded");
        let default = user_data.join("Default");
        let other = user_data.join("Profile 1");
        std::fs::create_dir_all(&default).expect("profile");
        std::fs::create_dir_all(&other).expect("profile");
        std::fs::write(
            default.join(PREFERENCES),
            br#"{"browser":{"window_placement":{"left":10}},"homepage":"about:blank"}"#,
        )
        .expect("prefs");
        std::fs::write(
            other.join(PREFERENCES),
            br#"{"browser":{"custom_chrome_frame":true}}"#,
        )
        .expect("prefs");

        let set = seed_user_data_dir(&user_data, &seeded).expect("seed");
        assert_eq!(set, vec![default.clone(), other.clone()]);
        assert_eq!(frame_pref(&default), Some(serde_json::Value::Bool(false)));
        let prefs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(default.join(PREFERENCES)).expect("read"))
                .expect("json");
        assert_eq!(prefs["homepage"], "about:blank");
        assert_eq!(prefs["browser"]["window_placement"]["left"], 10);
        assert_eq!(
            frame_pref(&other),
            Some(serde_json::Value::Bool(true)),
            "an explicit choice is kept"
        );

        // The person turns the custom frame back on: Chromium leaves the
        // default out of the file. A later launch must not undo that.
        std::fs::write(default.join(PREFERENCES), br#"{"browser":{}}"#).expect("prefs");
        let set = seed_user_data_dir(&user_data, &seeded).expect("seed");
        assert!(set.is_empty(), "each profile is seeded once: {set:?}");
        assert_eq!(frame_pref(&default), None);
    }

    #[test]
    fn a_fresh_browser_starts_with_the_system_title_bar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user_data = dir.path().join("chromium");
        let seeded = dir.path().join("state/seeded");
        let set = seed_user_data_dir(&user_data, &seeded).expect("seed");
        assert_eq!(set, vec![user_data.join("Default")]);
        assert_eq!(
            frame_pref(&user_data.join("Default")),
            Some(serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn a_running_browser_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user_data = dir.path().join("chromium");
        let default = user_data.join("Default");
        std::fs::create_dir_all(&default).expect("profile");
        std::fs::write(default.join(PREFERENCES), b"{}").expect("prefs");
        std::os::unix::fs::symlink("host-1234", user_data.join(SINGLETON_LOCK)).expect("lock");
        let seeded = dir.path().join("state/seeded");
        let set = seed_user_data_dir(&user_data, &seeded).expect("seed");
        assert!(set.is_empty());
        assert_eq!(frame_pref(&default), None);
        assert!(!seeded.exists(), "not counted as seeded");
    }
}
