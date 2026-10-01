//! The one directory an app may write to, and where it lives.
//!
//! An app does not name a writable path. It is *given* one: the
//! launcher decides the location, creates it, binds it read-write at a
//! fixed in-sandbox path, and points `HOME` and the `XDG_*` variables
//! inside it. Everything an app already does —
//! `cli::standalone_state_dir`, `dirs::home_dir()`, a hard-coded
//! `~/.local/share/<pkg>` — lands inside without changing a line, and an
//! app cannot opt out because there's no field to opt out with.
//!
//! ## Per-room partitioning falls out of the location
//!
//! The host location is `<data home>/tilo-rooms/<room>/<package>`. The
//! room is [`crate::sandbox::WORKSPACE_ID_ENV`], the same value the
//! collaboration cluster is keyed on, so two launches of one app into
//! two rooms get two directories, neither able to name the other's.
//!
//! A launch with no room gets the [`PERSONAL_ROOM`]: one directory like any
//! other, unreachable from a workspace.

use std::io;
use std::path::{Path, PathBuf};

/// Where the app's directory appears inside the sandbox. Fixed, so the
/// app never learns the host layout.
pub const SANDBOX_APP_DIR: &str = "/run/tilo/app";

/// Env var naming [`SANDBOX_APP_DIR`], so an app can be explicit about
/// "the directory the system gave me" rather than inferring it from `HOME`.
pub const APP_DIR_ENV: &str = "TILO_APP_DIR";

/// The room a launch with no workspace belongs to.
///
/// `docs/architecture/rooms.md`: "Your own room is not special — same kind of room,
/// one member." A slug like any other, deliberately not a UUID, so it
/// cannot collide with a workspace id.
pub const PERSONAL_ROOM: &str = "personal";

/// Directory under the user's data home that holds every room.
pub const ROOMS_DIR: &str = "tilo-rooms";

/// Turn a workspace id into a single path component that cannot escape
/// its parent.
///
/// The compositor passes a UUID, but the value is untrusted input like
/// any other. Everything outside `[A-Za-z0-9._-]` becomes `_`, which
/// removes `/` and `\0`, and the pure-dots cases (`.`, `..`) are
/// replaced outright since they name a directory rather than creating one.
pub fn room_slug(workspace_id: Option<&str>) -> String {
    let raw = workspace_id.unwrap_or("").trim();
    if raw.is_empty() {
        return PERSONAL_ROOM.to_string();
    }
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `.` and `..` survive the character filter and are the two names
    // that resolve somewhere other than a child.
    if mapped.chars().all(|c| c == '.') {
        return format!("room_{}", hex_digest(raw));
    }
    // A very long id would hit NAME_MAX and fail the mkdir with an
    // error that says nothing about rooms.
    if mapped.len() > 128 {
        return format!("room_{}", hex_digest(raw));
    }
    mapped
}

/// Short, stable, collision-resistant name for an id we refuse to use
/// verbatim. Not a security boundary — just a filename.
fn hex_digest(raw: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, raw.as_bytes());
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

/// The user's data home, as the *launcher's* own environment describes
/// it. Mirrors `cli::data_home` so host and in-sandbox sides agree.
pub fn host_data_home() -> Option<PathBuf> {
    xdg_paths::xdg::data_home()
}

/// Host location of one package's directory in one room.
pub fn host_app_dir(data_home: &Path, room: &str, package_id: &str) -> PathBuf {
    data_home
        .join(ROOMS_DIR)
        .join(room)
        .join(sanitise_component(package_id))
}

/// A package id is a reverse-DNS string from a manifest, untrusted for
/// the same reason a workspace id is.
fn sanitise_component(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() || mapped.chars().all(|c| c == '.') {
        return format!("pkg_{}", hex_digest(raw));
    }
    mapped
}

/// Create the directory this launch gets.
///
/// Returns the host path to bind at [`SANDBOX_APP_DIR`]. Errors are
/// real: an app with nowhere to write loses the user's work on exit, so
/// a failure here refuses the launch rather than starting something
/// that cannot save.
///
/// # Errors
///
/// Returns an error if the app directory or its `XDG_*` subdirectories
/// cannot be created.
pub fn prepare_host_app_dir(
    data_home: &Path,
    workspace_id: Option<&str>,
    package_id: &str,
) -> io::Result<PathBuf> {
    let room = room_slug(workspace_id);
    let dir = host_app_dir(data_home, &room, package_id);
    std::fs::create_dir_all(&dir)?;
    // The XDG tree the environment will point at, created up front so a
    // first-run write doesn't have to create its own parent.
    for sub in [".local/share", ".local/state", ".config", ".cache"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    remove_startup_traces_from_home(&dir);
    Ok(dir)
}

/// **Migration.** Startup traces used to be written at the top of the app
/// directory, which is the app's home: Files listed them, and the one being
/// written kept it repainting. They now go to `.local/state`; the old files
/// are diagnostics nothing reads, so they are removed rather than moved.
fn remove_startup_traces_from_home(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("startup-")
            && Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Bind the app's directory into the sandbox and point the environment
/// at it.
///
/// `HOME` is inside it on purpose: an app that writes `~/notes` is
/// doing the ordinary thing, and the answer to "where does that go" has
/// to be "somewhere the system chose", not the real home directory or
/// nowhere. Callers must splice this in before the trailing entrypoint
/// token.
pub fn push_app_dir(argv: &mut Vec<String>, host_dir: &Path) {
    argv.push("--bind".to_string());
    argv.push(host_dir.display().to_string());
    argv.push(SANDBOX_APP_DIR.to_string());
    for (key, value) in sandbox_env() {
        argv.push("--setenv".to_string());
        argv.push(key);
        argv.push(value);
    }
}

/// The environment [`push_app_dir`] sets. Split out so tests and the
/// Landlock rule builder can read it without parsing an argv.
pub fn sandbox_env() -> Vec<(String, String)> {
    let root = SANDBOX_APP_DIR;
    vec![
        (APP_DIR_ENV.to_string(), root.to_string()),
        ("HOME".to_string(), root.to_string()),
        ("XDG_DATA_HOME".to_string(), format!("{root}/.local/share")),
        ("XDG_STATE_HOME".to_string(), format!("{root}/.local/state")),
        ("XDG_CONFIG_HOME".to_string(), format!("{root}/.config")),
        ("XDG_CACHE_HOME".to_string(), format!("{root}/.cache")),
    ]
}

/// Where the session `settings.json` is visible after [`push_session_settings_bind`].
pub const SANDBOX_SESSION_SETTINGS: &str = "/run/tilo/app/.config/tilo/settings.json";

/// Overlay the session `~/.config/tilo` tree on the remapped XDG path so
/// Settings reads and writes the same document the compositor watches.
/// Writers replace `settings.json` by rename, so the containing directory
/// is bound, not the file inode.
pub fn push_session_settings_bind(argv: &mut Vec<String>, host_tilo_config: &Path) {
    argv.push("--bind".to_string());
    argv.push(host_tilo_config.display().to_string());
    argv.push(format!("{SANDBOX_APP_DIR}/.config/tilo"));
    argv.push("--setenv".to_string());
    argv.push("TILO_SESSION_SETTINGS".to_string());
    argv.push(SANDBOX_SESSION_SETTINGS.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_directory_paths_are_confined_per_room_and_package() {
        // Phase `a_room_name_can_never_escape_its_parent`.
        // The room name is the one path component an outside caller
        // supplies, so it is the one that has to be unable to escape.
        {
            let data = Path::new("/data");
            for hostile in [
                "..",
                "../..",
                "../../../etc",
                "/etc/shadow",
                "a/../../b",
                ".",
                "\0evil",
                "room\nname",
            ] {
                let slug = room_slug(Some(hostile));
                assert!(
                    !slug.contains('/'),
                    "{hostile:?} produced a multi-component slug {slug:?}"
                );
                assert!(
                    slug != "." && slug != "..",
                    "{hostile:?} produced {slug:?}, which names a directory \
                     instead of creating one"
                );
                let dir = host_app_dir(data, &slug, "so.tilo.notes");
                assert!(
                    dir.starts_with("/data/tilo-rooms/"),
                    "{hostile:?} escaped to {}",
                    dir.display()
                );
                // `..` inside the assembled path would let a later canonicalisation
                // walk out even though the string looks contained.
                assert!(
                    !dir.components().any(|c| c.as_os_str() == ".."),
                    "{hostile:?} left a parent component in {}",
                    dir.display()
                );
            }
        }
        // Phase `a_package_id_can_never_escape_its_room`.
        // Same argument for the package id: it comes out of a manifest, which
        // is the thing being distrusted.
        {
            let data = Path::new("/data");
            for hostile in ["..", "../../etc", "/", "a/b"] {
                let dir = host_app_dir(data, PERSONAL_ROOM, hostile);
                assert!(
                    dir.starts_with("/data/tilo-rooms/personal/"),
                    "{hostile:?} escaped to {}",
                    dir.display()
                );
                assert_eq!(
                    dir.components().count(),
                    Path::new("/data/tilo-rooms/personal/x")
                        .components()
                        .count(),
                    "{hostile:?} produced more than one component: {}",
                    dir.display()
                );
            }
        }
        // Phase `two_rooms_are_two_directories`.
        // Two rooms are two directories. This is the whole of per-room
        // partitioning at the filesystem layer.
        {
            let data = Path::new("/data");
            let a = host_app_dir(data, &room_slug(Some("room-a")), "so.tilo.notes");
            let b = host_app_dir(data, &room_slug(Some("room-b")), "so.tilo.notes");
            assert_ne!(a, b);
            assert!(!a.starts_with(&b) && !b.starts_with(&a));
        }
        // Phase `no_workspace_means_the_personal_room`.
        {
            assert_eq!(room_slug(None), PERSONAL_ROOM);
            assert_eq!(room_slug(Some("")), PERSONAL_ROOM);
            assert_eq!(room_slug(Some("   ")), PERSONAL_ROOM);
        }
    }

    #[test]
    fn app_directory_bind_and_xdg_tree() {
        // Phase `every_announced_path_is_inside_the_bound_directory`.
        // `XDG_DATA_HOME` has to land inside the bound directory or
        // `cli::standalone_state_dir` writes somewhere unbound and the app
        // silently loses its state.
        {
            for (key, value) in sandbox_env() {
                assert!(
                    value.starts_with(SANDBOX_APP_DIR),
                    "{key}={value} points outside {SANDBOX_APP_DIR}"
                );
            }
            let env = sandbox_env();
            let data = env
                .iter()
                .find(|(k, _)| k == "XDG_DATA_HOME")
                .map(|(_, v)| v.clone())
                .expect("XDG_DATA_HOME is announced");
            assert_eq!(data, "/run/tilo/app/.local/share");
        }
        // Phase `the_bind_is_read_write_at_the_fixed_path`.
        {
            let mut argv = Vec::new();
            push_app_dir(
                &mut argv,
                Path::new("/data/tilo-rooms/personal/so.tilo.notes"),
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--bind"
                    && w[1] == "/data/tilo-rooms/personal/so.tilo.notes"
                    && w[2] == SANDBOX_APP_DIR),
                "app dir must be bound read-write at the fixed path: {argv:?}"
            );
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--setenv" && w[1] == "HOME" && w[2] == SANDBOX_APP_DIR),
                "HOME must point inside the app dir: {argv:?}"
            );
        }
        // Phase `settings_session_store_overlays_the_remapped_xdg_path`.
        {
            let mut argv = Vec::new();
            push_session_settings_bind(&mut argv, Path::new("/home/ada/.config/tilo"));
            assert_eq!(
                argv,
                vec![
                    "--bind",
                    "/home/ada/.config/tilo",
                    "/run/tilo/app/.config/tilo",
                    "--setenv",
                    "TILO_SESSION_SETTINGS",
                    SANDBOX_SESSION_SETTINGS,
                ]
            );
        }
        // Phase `preparing_the_directory_creates_the_xdg_tree`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let dir = prepare_host_app_dir(tmp.path(), Some("room-a"), "so.tilo.notes").unwrap();
            assert!(dir.join(".local/share").is_dir());
            assert!(dir.join(".config").is_dir());
            assert!(dir.join(".cache").is_dir());
            assert_eq!(dir, tmp.path().join("tilo-rooms/room-a/so.tilo.notes"));
        }
        // Phase `startup_traces_leave_the_apps_home`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let dir = prepare_host_app_dir(tmp.path(), Some("room-a"), "so.tilo.notes").unwrap();
            std::fs::write(dir.join("startup-7.json"), "{}").unwrap();
            std::fs::write(dir.join("startup-notes.txt"), "mine").unwrap();
            std::fs::create_dir(dir.join("startup-album.json")).unwrap();
            prepare_host_app_dir(tmp.path(), Some("room-a"), "so.tilo.notes").unwrap();
            assert!(!dir.join("startup-7.json").exists());
            assert!(dir.join("startup-notes.txt").is_file(), "not a trace");
            assert!(
                dir.join("startup-album.json").is_dir(),
                "a directory is the user's"
            );
        }
    }
}
