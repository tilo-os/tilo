//! The one directory an app may write to, and where it lives.
//!
//! An app does not name a writable path. It is *given* one: the
//! launcher decides the location, creates it, binds it read-write at a
//! fixed in-sandbox path, and points `HOME` and the `XDG_*` variables
//! inside it. Everything an app already does — `cli::standalone_state_dir`,
//! `dirs::home_dir()`, a hard-coded `~/.local/share/<pkg>` — therefore
//! lands inside without the app changing a line, and an app cannot opt
//! out because there is no field it could write to opt out with.
//!
//! ## Per-room partitioning falls out of the location
//!
//! The host location is `<data home>/qos-rooms/<room>/<package>`. The
//! room is [`crate::sandbox::WORKSPACE_ID_ENV`] — the same value the
//! collaboration cluster is keyed on — so two launches of one app into
//! two rooms get two directories and neither can name the other's.
//! `plan/rooms.md` says "an app opened in a room starts empty"; before
//! this, that was true of the CRDT the app synced and false of the disk
//! it read, because `cli::standalone_state_dir` took only a package id.
//! Now it is one statement about one directory.
//!
//! A launch with no room — `qos-launcher run` from a shell, the CLI
//! shim, a test — gets the [`PERSONAL_ROOM`]. That is a real room and
//! not a bypass: it is one directory like any other, and an app launched
//! into a workspace cannot reach it.

use std::io;
use std::path::{Path, PathBuf};

/// Where the app's directory appears inside the sandbox. Fixed, so the
/// app never learns the host layout — the host path names the room and
/// the package, and neither is the app's business.
pub const SANDBOX_APP_DIR: &str = "/run/qos/app";

/// Env var naming [`SANDBOX_APP_DIR`] inside the sandbox. `HOME` and
/// the `XDG_*` variables point inside it too; this one exists so an app
/// can be explicit about "the directory the system gave me" rather than
/// inferring it from `HOME`.
pub const APP_DIR_ENV: &str = "QOS_APP_DIR";

/// The room a launch with no workspace belongs to.
///
/// `plan/rooms.md`: "Your own room is not special — same kind of room,
/// one member. It is where new records land by default." So it is a
/// slug like any other, and deliberately not a UUID, so it cannot
/// collide with a workspace id.
pub const PERSONAL_ROOM: &str = "personal";

/// Directory under the user's data home that holds every room.
pub const ROOMS_DIR: &str = "qos-rooms";

/// Marker written into an app directory that adopted pre-partition
/// state. Its presence is what makes the adoption happen exactly once;
/// its contents name where the bytes came from.
const ADOPTION_MARKER: &str = ".qos-adopted-from";

/// Turn a workspace id into a single path component that cannot escape
/// its parent.
///
/// The compositor passes a UUID, but the launcher is a program a user
/// can run, so the value is untrusted input like any other. Everything
/// outside `[A-Za-z0-9._-]` becomes `_`, which removes `/` and `\0` —
/// the only characters that could make this more than one component —
/// and the pure-dots cases (`.`, `..`) are replaced outright, because
/// they name a directory rather than creating one.
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
    // that would resolve somewhere other than a child. Anything else
    // starting with a dot is merely a hidden directory, which is fine.
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
    // FNV-1a, 64-bit. Avoids pulling a hash crate into the launcher for
    // a directory name.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The user's data home, as the *launcher's* own environment describes
/// it. Mirrors `cli::data_home` so the host side and the in-sandbox side
/// agree about what "data home" means.
pub fn host_data_home() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".local/share"))
}

/// Host location of one package's directory in one room.
pub fn host_app_dir(data_home: &Path, room: &str, package_id: &str) -> PathBuf {
    data_home
        .join(ROOMS_DIR)
        .join(room)
        .join(sanitise_component(package_id))
}

/// A package id is a reverse-DNS string from a manifest, so it is
/// untrusted for exactly the same reason a workspace id is.
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

/// Create (and migrate into, once) the directory this launch gets.
///
/// Returns the host path to bind at [`SANDBOX_APP_DIR`]. Errors are
/// real: an app with nowhere to write is an app that loses the user's
/// work on exit, so a failure here refuses the launch rather than
/// starting something that cannot save.
pub fn prepare_host_app_dir(
    data_home: &Path,
    workspace_id: Option<&str>,
    package_id: &str,
) -> io::Result<PathBuf> {
    let room = room_slug(workspace_id);
    let dir = host_app_dir(data_home, &room, package_id);
    std::fs::create_dir_all(&dir)?;
    // The XDG tree the environment will point at. Created up front so an
    // app that writes `$XDG_CONFIG_HOME/x` on first run does not have to
    // create the parent itself.
    for sub in [".local/share", ".local/state", ".config", ".cache"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    adopt_legacy_state_into_personal_room(data_home, &room, package_id, &dir);
    Ok(dir)
}

/// **Migration.** Copy pre-partition app state into the personal room,
/// once, leaving the original untouched.
///
/// Before app state was partitioned, every app kept its state document
/// at `<data home>/<package id>` — `cli::standalone_state_dir`, one
/// directory whichever room the app was opened into. That directory now
/// has a home: the personal room, which is where a launch with no
/// workspace already lands, so a user who has never used a second
/// workspace sees no change at all.
///
/// Three deliberate choices, all reversible:
///
/// - **Copy, not move.** Moving would make a downgrade lose data, and
///   the source is the only copy of things like a TOTP seed. The cost
///   is that the old directory is still on disk, and still holds
///   whatever it held — this migration does not un-write history, and
///   claiming otherwise would be the same false assurance as a bind
///   list that "proves" a sandbox.
/// - **Personal room only.** Copying into every room would put a user's
///   library in a workspace they may share, which is precisely the
///   exposure `plan/rooms.md` exists to remove. Bringing a record into
///   another room is a gesture the user makes (search, drag), not one a
///   launcher makes on their behalf.
/// - **Once.** The [`ADOPTION_MARKER`] records that it happened and
///   where from. A second launch does not re-copy, so edits made in the
///   room are never overwritten by a stale original.
///
/// **What it does not adopt.** Only `<data home>/<package id>` — the one
/// directory `cli::standalone_state_dir` resolves to, which is where
/// `app_state::DocStore` keeps a package's document and its
/// `records/<key>/` subtree. That is the state the per-record work put on
/// disk, so it is the part that matters. Many apps *also* used ad-hoc
/// locations of their own choosing (`~/.local/share/qos-notes`,
/// `~/.config/qos-browser`, `~/.cache/qos-maps`, …). Those are left
/// where they are and the app will not see them. Adopting them would
/// mean the launcher holding a per-package list of paths, which is the
/// thing this whole change removes — re-introducing it through the
/// migration door would be no better than leaving it in the manifest.
///
/// To reverse it: delete `<data home>/qos-rooms/personal/<package>`.
/// The original at `<data home>/<package>` was never touched.
fn adopt_legacy_state_into_personal_room(
    data_home: &Path,
    room: &str,
    package_id: &str,
    app_dir: &Path,
) {
    if room != PERSONAL_ROOM {
        return;
    }
    let marker = app_dir.join(ADOPTION_MARKER);
    if marker.exists() {
        return;
    }
    let legacy = data_home.join(sanitise_component(package_id));
    // Guard against the rooms tree adopting itself, which a package id
    // of `qos-rooms` would otherwise arrange.
    if !legacy.is_dir() || legacy.starts_with(data_home.join(ROOMS_DIR)) {
        let _ = std::fs::write(&marker, b"nothing to adopt\n");
        return;
    }
    let dest = app_dir.join(".local/share").join(sanitise_component(package_id));
    if dest.exists() {
        let _ = std::fs::write(&marker, b"destination already present\n");
        return;
    }
    match copy_tree(&legacy, &dest) {
        Ok(n) => {
            tracing::info!(
                target: "qos_launcher::appdir",
                package = %package_id,
                from = %legacy.display(),
                to = %dest.display(),
                entries = n,
                "adopted pre-partition app state into the personal room \
                 (the original was copied, not moved)",
            );
            let _ = std::fs::write(
                &marker,
                format!("{}\n", legacy.display()).as_bytes(),
            );
        }
        Err(e) => {
            // A failed adoption must not refuse the launch: the app
            // still has a working, empty directory. Losing the old
            // library is visible; losing the app is worse.
            tracing::warn!(
                target: "qos_launcher::appdir",
                package = %package_id,
                from = %legacy.display(),
                "could not adopt pre-partition app state: {e}",
            );
            let _ = std::fs::remove_dir_all(&dest);
        }
    }
}

/// Recursive copy that recreates symlinks as symlinks rather than
/// following them — following one would copy whatever it pointed at,
/// which for a link into `/` is the whole filesystem.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<usize> {
    let mut count = 0usize;
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if kind.is_symlink() {
            let target = std::fs::read_link(&from)?;
            let _ = std::os::unix::fs::symlink(target, &to);
            count += 1;
        } else if kind.is_dir() {
            count += copy_tree(&from, &to)?;
        } else if kind.is_file() {
            std::fs::copy(&from, &to)?;
            count += 1;
        }
        // Sockets, fifos and device nodes are not state; skipping them
        // is not a loss and copying them is not possible.
    }
    Ok(count)
}

/// Bind the app's directory into the sandbox and point the environment
/// at it.
///
/// `HOME` is inside it on purpose. An app that writes `~/notes` is not
/// misbehaving — it is doing the ordinary thing — and the answer to
/// "where does that go" has to be "somewhere the system chose", not
/// "the real home directory" and not "nowhere, so the app crashes".
///
/// Callers must splice this in *before* the trailing entrypoint token.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The room name is the one path component an outside caller
    /// supplies, so it is the one that has to be unable to escape.
    #[test]
    fn a_room_name_can_never_escape_its_parent() {
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
            let dir = host_app_dir(data, &slug, "dev.qos.notes");
            assert!(
                dir.starts_with("/data/qos-rooms/"),
                "{hostile:?} escaped to {}",
                dir.display()
            );
            // `..` inside the assembled path would let a later
            // canonicalisation walk out even though the string looks
            // contained.
            assert!(
                !dir.components().any(|c| c.as_os_str() == ".."),
                "{hostile:?} left a parent component in {}",
                dir.display()
            );
        }
    }

    /// Same argument for the package id: it comes out of a manifest,
    /// and the manifest is the thing being distrusted.
    #[test]
    fn a_package_id_can_never_escape_its_room() {
        let data = Path::new("/data");
        for hostile in ["..", "../../etc", "/", "a/b"] {
            let dir = host_app_dir(data, PERSONAL_ROOM, hostile);
            assert!(
                dir.starts_with("/data/qos-rooms/personal/"),
                "{hostile:?} escaped to {}",
                dir.display()
            );
            assert_eq!(
                dir.components().count(),
                Path::new("/data/qos-rooms/personal/x").components().count(),
                "{hostile:?} produced more than one component: {}",
                dir.display()
            );
        }
    }

    /// Two rooms are two directories. This is the whole of per-room
    /// partitioning at the filesystem layer.
    #[test]
    fn two_rooms_are_two_directories() {
        let data = Path::new("/data");
        let a = host_app_dir(data, &room_slug(Some("room-a")), "dev.qos.notes");
        let b = host_app_dir(data, &room_slug(Some("room-b")), "dev.qos.notes");
        assert_ne!(a, b);
        assert!(!a.starts_with(&b) && !b.starts_with(&a));
    }

    #[test]
    fn no_workspace_means_the_personal_room() {
        assert_eq!(room_slug(None), PERSONAL_ROOM);
        assert_eq!(room_slug(Some("")), PERSONAL_ROOM);
        assert_eq!(room_slug(Some("   ")), PERSONAL_ROOM);
    }

    /// `XDG_DATA_HOME` has to land inside the bound directory or
    /// `cli::standalone_state_dir` writes somewhere that is not bound
    /// and the app silently loses its state.
    #[test]
    fn every_announced_path_is_inside_the_bound_directory() {
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
        assert_eq!(data, "/run/qos/app/.local/share");
    }

    #[test]
    fn the_bind_is_read_write_at_the_fixed_path() {
        let mut argv = Vec::new();
        push_app_dir(&mut argv, Path::new("/data/qos-rooms/personal/dev.qos.notes"));
        assert!(
            argv.windows(3).any(|w| w[0] == "--bind"
                && w[1] == "/data/qos-rooms/personal/dev.qos.notes"
                && w[2] == SANDBOX_APP_DIR),
            "app dir must be bound read-write at the fixed path: {argv:?}"
        );
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--setenv" && w[1] == "HOME" && w[2] == SANDBOX_APP_DIR),
            "HOME must point inside the app dir: {argv:?}"
        );
    }

    #[test]
    fn preparing_the_directory_creates_the_xdg_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = prepare_host_app_dir(tmp.path(), Some("room-a"), "dev.qos.notes").unwrap();
        assert!(dir.join(".local/share").is_dir());
        assert!(dir.join(".config").is_dir());
        assert!(dir.join(".cache").is_dir());
        assert_eq!(dir, tmp.path().join("qos-rooms/room-a/dev.qos.notes"));
    }

    /// The migration adopts into the personal room and nowhere else.
    #[test]
    fn legacy_state_is_adopted_into_the_personal_room_only() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("dev.qos.notes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("state.bin"), b"old library").unwrap();

        let workspace =
            prepare_host_app_dir(tmp.path(), Some("room-a"), "dev.qos.notes").unwrap();
        assert!(
            !workspace.join(".local/share/dev.qos.notes/state.bin").exists(),
            "a workspace room must start empty"
        );

        let personal = prepare_host_app_dir(tmp.path(), None, "dev.qos.notes").unwrap();
        let adopted = personal.join(".local/share/dev.qos.notes/state.bin");
        assert_eq!(
            std::fs::read(&adopted).unwrap(),
            b"old library",
            "the personal room adopts pre-partition state"
        );
        assert!(
            legacy.join("state.bin").exists(),
            "the original is copied, not moved"
        );
    }

    /// A second launch must not overwrite what the user did in the room
    /// with a stale copy of the original.
    #[test]
    fn adoption_happens_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("dev.qos.notes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("state.bin"), b"old").unwrap();

        let personal = prepare_host_app_dir(tmp.path(), None, "dev.qos.notes").unwrap();
        let adopted = personal.join(".local/share/dev.qos.notes/state.bin");
        std::fs::write(&adopted, b"edited in the room").unwrap();

        prepare_host_app_dir(tmp.path(), None, "dev.qos.notes").unwrap();
        assert_eq!(
            std::fs::read(&adopted).unwrap(),
            b"edited in the room",
            "a re-launch must not re-copy over the user's edits"
        );
    }

    /// A symlink in the legacy tree is copied as a link. Following it
    /// would pull whatever it names into the app's directory.
    #[test]
    fn adoption_does_not_follow_symlinks_out_of_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside-secret");
        std::fs::write(&outside, b"not the app's").unwrap();
        let legacy = tmp.path().join("dev.qos.notes");
        std::fs::create_dir_all(&legacy).unwrap();
        std::os::unix::fs::symlink(&outside, legacy.join("link")).unwrap();

        let personal = prepare_host_app_dir(tmp.path(), None, "dev.qos.notes").unwrap();
        let copied = personal.join(".local/share/dev.qos.notes/link");
        assert!(
            copied.symlink_metadata().unwrap().file_type().is_symlink(),
            "a symlink must be copied as a symlink, not resolved"
        );
    }
}
