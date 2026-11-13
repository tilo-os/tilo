//! The one directory an app may write to, and where it lives.
//!
//! An app does not name its writable path; the launcher resolves one per app and
//! workspace.

use std::io;
use std::path::{Path, PathBuf};

/// Where the app's directory appears inside the sandbox.
pub const SANDBOX_APP_DIR: &str = "/run/tilo/app";

/// Env var naming [`SANDBOX_APP_DIR`], so an app can be explicit about "the directory the system gave
/// me" rather than inferring it from `HOME`.
pub const APP_DIR_ENV: &str = "TILO_APP_DIR";

/// The directory a launch with no workspace belongs to.
pub const PERSONAL_WORKSPACE: &str = "personal";

/// Directory under the user's data home that holds every workspace.
pub const WORKSPACES_DIR: &str = "tilo-workspaces";

/// Directory under the user's data home that holds each app's records of the person's own, shared by
/// the app's launches in every workspace.
pub const PERSON_DIR: &str = "tilo-person";

/// Where the app's person directory appears inside the sandbox.
pub const SANDBOX_PERSON_DIR: &str = "/run/tilo/person";

/// Env var naming [`SANDBOX_PERSON_DIR`]; the toolkit keeps the person's records there
/// (`ui::application::PERSON_DIR_ENV`, the same name).
pub const PERSON_DIR_ENV: &str = "TILO_PERSON_DIR";

/// Where the shared font index appears inside the sandbox.
pub const SANDBOX_FONT_INDEX: &str = "/run/tilo/font-index";

/// Directory under the user's data home that holds the font index every app of a build shares, beside
/// [`WORKSPACES_DIR`] and [`PERSON_DIR`].
pub const FONT_INDEX_DIR: &str = "tilo-font-index";

/// Create the shared font index directory and answer where it is: one per user, so the second app
/// opened on the device reads the database the first one's launch built instead of parsing every face
/// again.
pub fn prepare_host_font_index() -> Option<PathBuf> {
    let dir = tilo_paths::xdg::data_home()?.join(FONT_INDEX_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Bind that directory into the sandbox and name it to the app.
pub fn push_font_index(argv: &mut Vec<String>, dir: &Path) {
    argv.push("--bind".to_string());
    argv.push(dir.display().to_string());
    argv.push(SANDBOX_FONT_INDEX.to_string());
    argv.push("--setenv".to_string());
    argv.push(tilo_process::environment::FONT_INDEX_ENV.to_string());
    argv.push(SANDBOX_FONT_INDEX.to_string());
}

/// The Landlock grant for [`push_font_index`]'s bind, read and write: a bound tree the ruleset does not
/// name is visible and unreadable.
pub fn push_font_index_landlock(argv: &mut Vec<String>) {
    argv.push("--rw".to_string());
    argv.push(SANDBOX_FONT_INDEX.to_string());
}

/// Turn a workspace id into a single path component that cannot escape its parent.
pub fn workspace_slug(workspace_id: Option<&str>) -> String {
    let raw = workspace_id.unwrap_or("").trim();
    if raw.is_empty() {
        return PERSONAL_WORKSPACE.to_string();
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
    // `.` and `..` survive the character filter and are the two names that resolve somewhere other
    // than a child.
    if mapped.chars().all(|c| c == '.') {
        return format!("workspace_{}", hex_digest(raw));
    }
    // A very long id would hit NAME_MAX and fail the mkdir with an error that says nothing about
    // workspaces.
    if mapped.len() > 128 {
        return format!("workspace_{}", hex_digest(raw));
    }
    mapped
}

/// Short, stable, collision-resistant name for an id we refuse to use verbatim.
fn hex_digest(raw: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, raw.as_bytes());
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

/// The user's data home, as the *launcher's* own environment describes it.
pub fn host_data_home() -> Option<PathBuf> {
    tilo_paths::xdg::data_home()
}

/// Host location of one package's directory in one workspace.
pub fn host_app_dir(data_home: &Path, workspace: &str, package_id: &str) -> PathBuf {
    data_home
        .join(WORKSPACES_DIR)
        .join(workspace)
        .join(sanitise_component(package_id))
}

/// A package id is a reverse-DNS string from a manifest, untrusted for the same reason a workspace id
/// is.
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
pub fn prepare_host_app_dir(
    data_home: &Path,
    workspace_id: Option<&str>,
    package_id: &str,
) -> io::Result<PathBuf> {
    let workspace = workspace_slug(workspace_id);
    let dir = host_app_dir(data_home, &workspace, package_id);
    std::fs::create_dir_all(&dir)?;
    // The XDG tree the environment will point at, created up front so a first-run write doesn't have
    // to create its own parent.
    for sub in [
        ".local",
        ".local/share",
        ".local/state",
        ".config",
        ".cache",
    ] {
        let path = dir.join(sub);
        refuse_symlink(&path)?;
        std::fs::create_dir_all(&path)?;
    }
    remove_startup_traces_from_home(&dir);
    Ok(dir)
}

/// Prepare an owner-private service directory in a fresh private working lease.
#[cfg(target_os = "linux")]
pub fn prepare_private_host_app_dir(
    data_home: &Path,
    workspace_id: Option<&str>,
    package_id: &str,
) -> io::Result<PathBuf> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let open = |path: &Path| {
        std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
    };
    let root = open(data_home)?;
    let original = root.metadata()?;
    let uid = rustix::process::geteuid().as_raw();
    if original.uid() != uid || original.mode() & 0o077 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if std::fs::read_dir(data_home)?.next().transpose()?.is_some() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let dir = prepare_host_app_dir(data_home, workspace_id, package_id)?;
    let file = open(&dir)?;
    if file.metadata()?.uid() != uid {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    let actual = file.metadata()?;
    let current_root = open(data_home)?.metadata()?;
    let current_dir = open(&dir)?.metadata()?;
    if actual.uid() != uid
        || actual.mode() & 0o777 != 0o700
        || (
            current_root.dev(),
            current_root.ino(),
            current_root.uid(),
            current_root.mode(),
        ) != (
            original.dev(),
            original.ino(),
            original.uid(),
            original.mode(),
        )
        || (current_dir.dev(), current_dir.ino()) != (actual.dev(), actual.ino())
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(dir)
}

/// Fails when `path` is a symbolic link, which only the sandboxed app could have put there.
fn refuse_symlink(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is a symbolic link; the app directory's XDG tree must be real directories",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// Host location of one package's person directory: the same for every workspace it is launched into.
pub fn host_person_dir(data_home: &Path, package_id: &str) -> PathBuf {
    data_home
        .join(PERSON_DIR)
        .join(sanitise_component(package_id))
}

/// Create the person directory this launch binds.
pub fn prepare_host_person_dir(data_home: &Path, package_id: &str) -> io::Result<PathBuf> {
    let dir = host_person_dir(data_home, package_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Bind the person directory into the sandbox and name it to the app.
pub fn push_person_dir(argv: &mut Vec<String>, host_dir: &Path) {
    argv.push("--bind".to_string());
    argv.push(host_dir.display().to_string());
    argv.push(SANDBOX_PERSON_DIR.to_string());
    argv.push("--setenv".to_string());
    argv.push(PERSON_DIR_ENV.to_string());
    argv.push(SANDBOX_PERSON_DIR.to_string());
}

/// **Migration.** Startup traces used to be written at the top of the app directory, which is the app's
/// home: Files listed them, and the one being written kept it repainting.
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

/// Bind the app's directory into the sandbox and point the environment at it.
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

/// The environment [`push_app_dir`] sets.
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

/// Where Settings sees the session's configuration root, the remapped `$XDG_CONFIG_HOME/tilo`.
pub const SANDBOX_SESSION_CONFIG: &str = "/run/tilo/app/.config/tilo";

/// How one part of the session's configuration reaches Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBind {
    /// A directory Settings writes, bound read-write.
    Writable,
    /// A file Settings reads, bound read-only when it exists.
    ReadOnly,
}

/// The parts of `~/.config/tilo` Settings reaches, by path under that root, and nothing else of it: not
/// the settings document or its records (Settings reads and writes the session's settings through the
/// shell's settings service on its collaboration channel, as every app does, with the reach the image
/// grants the device's settings app), not the identity store (records, root secrets, the active
/// selector, device fleets — the host administers them behind its `tilo.identity` actions), the trust
/// and revocation lists, the app registries, or a file added there later.
pub const SESSION_SETTINGS_BINDS: &[(&str, SessionBind)] = &[
    ("accounts", SessionBind::Writable),
    ("state.json", SessionBind::ReadOnly),
    ("continuity-provenance.json", SessionBind::ReadOnly),
];

/// Bind the parts of the session's `~/.config/tilo` (`host_tilo_config`) Settings reaches
/// ([`SESSION_SETTINGS_BINDS`]) over its remapped XDG path.
pub fn push_session_settings_bind(argv: &mut Vec<String>, host_tilo_config: &Path) {
    for (relative, bind) in SESSION_SETTINGS_BINDS {
        let flag = match bind {
            SessionBind::Writable => "--bind",
            SessionBind::ReadOnly => "--ro-bind-try",
        };
        argv.push(flag.to_string());
        argv.push(host_tilo_config.join(relative).display().to_string());
        argv.push(format!("{SANDBOX_SESSION_CONFIG}/{relative}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symlinked_xdg_component_refuses_the_launch() {
        let data = tempfile::tempdir().expect("data home");
        let dir = prepare_host_app_dir(data.path(), Some("workspace"), "so.tilo.notes")
            .expect("a fresh app directory");
        let elsewhere = tempfile::tempdir().expect("a directory the app must not reach");
        std::fs::remove_dir_all(dir.join(".local")).expect("remove");
        std::os::unix::fs::symlink(elsewhere.path(), dir.join(".local")).expect("plant");
        let error = prepare_host_app_dir(data.path(), Some("workspace"), "so.tilo.notes")
            .expect_err("a symlinked component must refuse the launch");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert!(
            std::fs::read_dir(elsewhere.path())
                .expect("read")
                .next()
                .is_none(),
            "nothing may be created through the link"
        );
    }

    #[test]
    fn app_directory_paths_are_confined_per_workspace_and_package() {
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
                "workspace\nname",
            ] {
                let slug = workspace_slug(Some(hostile));
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
                    dir.starts_with("/data/tilo-workspaces/"),
                    "{hostile:?} escaped to {}",
                    dir.display()
                );
                // `..` inside the assembled path would let a later canonicalisation walk out even
                // though the string looks contained.
                assert!(
                    !dir.components().any(|c| c.as_os_str() == ".."),
                    "{hostile:?} left a parent component in {}",
                    dir.display()
                );
            }
        }
        {
            let data = Path::new("/data");
            for hostile in ["..", "../../etc", "/", "a/b"] {
                let dir = host_app_dir(data, PERSONAL_WORKSPACE, hostile);
                assert!(
                    dir.starts_with("/data/tilo-workspaces/personal/"),
                    "{hostile:?} escaped to {}",
                    dir.display()
                );
                assert_eq!(
                    dir.components().count(),
                    Path::new("/data/tilo-workspaces/personal/x")
                        .components()
                        .count(),
                    "{hostile:?} produced more than one component: {}",
                    dir.display()
                );
            }
        }
        {
            let data = Path::new("/data");
            let a = host_app_dir(data, &workspace_slug(Some("workspace-a")), "so.tilo.notes");
            let b = host_app_dir(data, &workspace_slug(Some("workspace-b")), "so.tilo.notes");
            assert_ne!(a, b);
            assert!(!a.starts_with(&b) && !b.starts_with(&a));
        }
        {
            assert_eq!(workspace_slug(None), PERSONAL_WORKSPACE);
            assert_eq!(workspace_slug(Some("")), PERSONAL_WORKSPACE);
            assert_eq!(workspace_slug(Some("   ")), PERSONAL_WORKSPACE);
        }
    }

    /// Every app of a build reads and writes one font index, bound where the launcher names it, not in
    /// its own home.
    #[test]
    fn the_font_index_is_one_directory_for_every_app() {
        let mut argv = Vec::new();
        push_font_index(
            &mut argv,
            Path::new("/home/ada/.local/share/tilo-font-index"),
        );
        assert_eq!(
            argv,
            vec![
                "--bind",
                "/home/ada/.local/share/tilo-font-index",
                SANDBOX_FONT_INDEX,
                "--setenv",
                tilo_process::environment::FONT_INDEX_ENV,
                SANDBOX_FONT_INDEX,
            ]
        );
        let mut rules = Vec::new();
        push_font_index_landlock(&mut rules);
        assert_eq!(
            rules,
            vec!["--rw", SANDBOX_FONT_INDEX],
            "the bind is writable"
        );
    }

    #[test]
    fn app_directory_bind_and_xdg_tree() {
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
        {
            let mut argv = Vec::new();
            push_app_dir(
                &mut argv,
                Path::new("/data/tilo-workspaces/personal/so.tilo.notes"),
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--bind"
                    && w[1] == "/data/tilo-workspaces/personal/so.tilo.notes"
                    && w[2] == SANDBOX_APP_DIR),
                "app dir must be bound read-write at the fixed path: {argv:?}"
            );
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--setenv" && w[1] == "HOME" && w[2] == SANDBOX_APP_DIR),
                "HOME must point inside the app dir: {argv:?}"
            );
        }
        {
            let mut argv = Vec::new();
            push_session_settings_bind(&mut argv, Path::new("/home/ada/.config/tilo"));
            assert_eq!(
                argv,
                vec![
                    "--bind",
                    "/home/ada/.config/tilo/accounts",
                    "/run/tilo/app/.config/tilo/accounts",
                    "--ro-bind-try",
                    "/home/ada/.config/tilo/state.json",
                    "/run/tilo/app/.config/tilo/state.json",
                    "--ro-bind-try",
                    "/home/ada/.config/tilo/continuity-provenance.json",
                    "/run/tilo/app/.config/tilo/continuity-provenance.json",
                ]
            );
            assert!(
                !argv.iter().any(|arg| arg.contains("/records")),
                "the settings document reaches Settings through the shell's settings service, \
                 never a bind: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg == "/home/ada/.config/tilo"),
                "the configuration root itself is never bound: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg.contains("/identity")),
                "the identity store is the host's, never Settings': {argv:?}"
            );
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let dir =
                prepare_host_app_dir(tmp.path(), Some("workspace-a"), "so.tilo.notes").unwrap();
            assert!(dir.join(".local/share").is_dir());
            assert!(dir.join(".config").is_dir());
            assert!(dir.join(".cache").is_dir());
            assert_eq!(
                dir,
                tmp.path().join("tilo-workspaces/workspace-a/so.tilo.notes")
            );
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let dir =
                prepare_host_app_dir(tmp.path(), Some("workspace-a"), "so.tilo.notes").unwrap();
            std::fs::write(dir.join("startup-7.json"), "{}").unwrap();
            std::fs::write(dir.join("startup-notes.txt"), "mine").unwrap();
            std::fs::create_dir(dir.join("startup-album.json")).unwrap();
            prepare_host_app_dir(tmp.path(), Some("workspace-a"), "so.tilo.notes").unwrap();
            assert!(!dir.join("startup-7.json").exists());
            assert!(dir.join("startup-notes.txt").is_file(), "not a trace");
            assert!(
                dir.join("startup-album.json").is_dir(),
                "a directory is the user's"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod private_service_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn private_service_directory_survives_canonical_launch_preparation() {
        let working = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let package = "so.tilo.imager.home-export";
        let dir = prepare_private_host_app_dir(working.path(), None, package).unwrap();
        assert_eq!(
            dir,
            host_app_dir(working.path(), PERSONAL_WORKSPACE, package)
        );
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(&dir).unwrap().uid(),
            rustix::process::geteuid().as_raw()
        );
        std::fs::write(dir.join("provider-state"), b"private state").unwrap();
        assert_eq!(
            prepare_host_app_dir(working.path(), None, package).unwrap(),
            dir
        );
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::read(dir.join("provider-state")).unwrap(),
            b"private state"
        );
        assert!(dir.join(".local/share").is_dir());
        assert!(dir.join(".config").is_dir());
    }

    #[test]
    fn private_service_directory_refuses_nonprivate_and_redirected_working_leases() {
        let working = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::set_permissions(working.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            prepare_private_host_app_dir(working.path(), None, "so.test")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_dir(working.path()).unwrap().count(), 0);
        std::fs::set_permissions(working.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(outside.path().join("personal"), b"unchanged").unwrap();
        std::os::unix::fs::symlink(outside.path(), working.path().join(WORKSPACES_DIR)).unwrap();
        assert_eq!(
            prepare_private_host_app_dir(working.path(), None, "so.test")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(outside.path().join("personal")).unwrap(),
            b"unchanged"
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }
}
