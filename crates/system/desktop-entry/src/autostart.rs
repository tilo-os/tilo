//! XDG autostart: the `.desktop` files a session starts at login.
//!
//! The Desktop Application Autostart Specification looks in
//! `$XDG_CONFIG_HOME/autostart` and then each `$XDG_CONFIG_DIRS/autostart`. A file
//! name is the identity: the first directory holding `foo.desktop` decides it, so a
//! user file overrides (or, with `Hidden=true`, cancels) the system one. An entry
//! is started only if it is an application, is not hidden, is shown in this desktop
//! (`OnlyShowIn`/`NotShowIn`), has not been switched off with
//! `X-GNOME-Autostart-enabled=false`, and its `TryExec` program exists.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::entry::{DesktopEntry, EntryType, parse};
use crate::locale::locale_candidates;

/// The autostart directories of the current environment, most important first.
pub fn autostart_dirs() -> Vec<PathBuf> {
    autostart_dirs_from(
        xdg_paths::xdg::config_home().as_deref(),
        &xdg_paths::xdg::config_dirs(),
    )
}

/// [`autostart_dirs`] from explicit base directories.
pub fn autostart_dirs_from(config_home: Option<&Path>, config_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for base in config_home
        .map(Path::to_path_buf)
        .into_iter()
        .chain(config_dirs.iter().cloned())
    {
        let dir = base.join("autostart");
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Why an autostart file is not started — reported so a session log can explain a
/// program that "did not come up".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    NotAnApplication,
    Hidden,
    NotShownIn,
    Disabled,
    TryExecMissing(String),
    NoExec,
}

/// One scan's outcome: what to start, and what was deliberately left out.
#[derive(Debug, Default, Clone)]
pub struct AutostartScan {
    pub start: Vec<DesktopEntry>,
    pub skipped: Vec<(PathBuf, Skipped)>,
}

/// Scan `dirs` for entries to start in `desktops` (the `XDG_CURRENT_DESKTOP` names).
/// `program_exists` answers `TryExec`, so tests need no real binaries.
pub fn scan_autostart(
    dirs: &[PathBuf],
    desktops: &[String],
    program_exists: impl Fn(&str) -> bool,
) -> AutostartScan {
    let locales = locale_candidates();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = AutostartScan::default();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = read
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        files.sort();
        for path in files {
            let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            // The more important directory already decided this name, even if it
            // decided "do not start".
            if !seen.insert(file_name.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let id = file_name.trim_end_matches(".desktop");
            let Some(entry) = parse(&text, id, &path, &locales) else {
                continue;
            };
            match why_skipped(&entry, desktops, &program_exists) {
                Some(reason) => out.skipped.push((path, reason)),
                None => out.start.push(entry),
            }
        }
    }
    out
}

fn why_skipped(
    entry: &DesktopEntry,
    desktops: &[String],
    program_exists: &impl Fn(&str) -> bool,
) -> Option<Skipped> {
    if entry.entry_type != EntryType::Application {
        return Some(Skipped::NotAnApplication);
    }
    if entry.hidden {
        return Some(Skipped::Hidden);
    }
    if !entry.shows_in(desktops) {
        return Some(Skipped::NotShownIn);
    }
    if entry.autostart_enabled == Some(false) {
        return Some(Skipped::Disabled);
    }
    if let Some(program) = entry.try_exec.as_deref()
        && !program_exists(program)
    {
        return Some(Skipped::TryExecMissing(program.to_string()));
    }
    if entry.exec.is_none() {
        return Some(Skipped::NoExec);
    }
    None
}

/// `TryExec` per the specification: an absolute path must be an executable file; a bare
/// name is looked up on `PATH`.
pub fn program_on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let executable = |path: &Path| {
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    let program_path = Path::new(program);
    if program_path.is_absolute() {
        return executable(program_path);
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(program))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn app(extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=App\nExec=app\n{extra}")
    }

    #[test]
    fn dirs_follow_the_specification_order() {
        assert_eq!(
            autostart_dirs_from(
                Some(Path::new("/home/u/.config")),
                &[PathBuf::from("/etc/xdg"), PathBuf::from("/etc/xdg")]
            ),
            vec![
                PathBuf::from("/home/u/.config/autostart"),
                PathBuf::from("/etc/xdg/autostart")
            ]
        );
    }

    #[test]
    fn user_files_override_system_files_and_every_filter_applies() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("home/autostart");
        let system = root.path().join("etc/autostart");
        write(&system, "overridden.desktop", &app(""));
        write(&user, "overridden.desktop", &app("Hidden=true\n"));
        write(&system, "plain.desktop", &app(""));
        write(&system, "gnome-only.desktop", &app("OnlyShowIn=GNOME;\n"));
        write(&system, "not-tilo.desktop", &app("NotShowIn=Tilo;\n"));
        write(&system, "tilo-only.desktop", &app("OnlyShowIn=tilo;\n"));
        write(
            &system,
            "off.desktop",
            &app("X-GNOME-Autostart-enabled=false\n"),
        );
        write(
            &system,
            "tryexec.desktop",
            &app("TryExec=/nonexistent/helper\n"),
        );
        write(&system, "found.desktop", &app("TryExec=present\n"));
        write(
            &system,
            "link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=x\n",
        );
        write(&system, "readme.txt", "not a desktop file");

        let scan = scan_autostart(&[user, system], &["tilo".to_string()], |program| {
            program == "present"
        });
        let started: Vec<&str> = scan.start.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(started, vec!["found", "plain", "tilo-only"]);
        let reason = |id: &str| {
            scan.skipped
                .iter()
                .find(|(path, _)| path.file_stem().is_some_and(|stem| stem == id))
                .map(|(_, reason)| reason.clone())
        };
        assert_eq!(reason("overridden"), Some(Skipped::Hidden));
        assert_eq!(reason("gnome-only"), Some(Skipped::NotShownIn));
        assert_eq!(reason("not-tilo"), Some(Skipped::NotShownIn));
        assert_eq!(reason("off"), Some(Skipped::Disabled));
        assert_eq!(
            reason("tryexec"),
            Some(Skipped::TryExecMissing("/nonexistent/helper".into()))
        );
        assert_eq!(reason("link"), Some(Skipped::NotAnApplication));
    }

    #[test]
    fn missing_directories_are_simply_empty() {
        let scan = scan_autostart(&[PathBuf::from("/nonexistent/autostart")], &[], |_| true);
        assert!(scan.start.is_empty() && scan.skipped.is_empty());
    }
}
