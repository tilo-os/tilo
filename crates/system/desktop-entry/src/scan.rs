//! Finding every application entry installed on the host.
//!
//! The search path is the XDG one: `$XDG_DATA_HOME/applications` first, then each
//! `$XDG_DATA_DIRS` entry's `applications` in order. Flatpak's two export roots are appended
//! when the environment did not already name them — a desktop session usually puts them in
//! `XDG_DATA_DIRS` itself, but a compositor started from a bare service manager may not have
//! inherited that, and the apps are installed either way.
//!
//! ## Precedence
//!
//! Entries are keyed by desktop-file ID — the path relative to the `applications` directory
//! with `/` folded to `-`. The **first** directory in the search order that provides an ID
//! wins, and a later one is discarded — what makes a user's
//! `~/.local/share/applications/foo.desktop` override the system's. A `Hidden=true` entry is
//! the spec's tombstone; it wins the same way and then removes the ID entirely.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::entry::{DesktopEntry, EntrySource, EntryType, parse};
use crate::locale::locale_candidates;

/// `XDG_CURRENT_DESKTOP` this desktop announces, and the name `OnlyShowIn` / `NotShowIn`
/// are matched against.
pub const CURRENT_DESKTOP: &str = "tilo";
/// Desktops Tilo is compatible with for menu visibility. GNOME-only
/// applications would otherwise vanish because we do not *be* GNOME. Menus only:
/// autostart matches Tilo's own name, see [`crate::autostart::AUTOSTART_DESKTOPS`].
pub fn current_desktops() -> Vec<String> {
    vec![CURRENT_DESKTOP.to_string(), "GNOME".to_string()]
}

/// What a scan turned up, plus what it declined and why. The counters are not decoration:
/// "the app I installed is not in the palette" has half a dozen legitimate causes
/// (`NoDisplay`, an unresolved `TryExec`, a desktop filter, a tombstone), and reporting only
/// the survivors couldn't distinguish any of them from a scanner bug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// `.desktop` files read.
    pub files_seen: usize,
    /// Files that did not parse as a desktop entry at all.
    pub unparseable: usize,
    /// Dropped because a higher-precedence directory already provided the same ID.
    pub shadowed: usize,
    /// A differently-named desktop file described the same visible
    /// application. Packaging aliases are common when an app moves between
    /// distribution and Flatpak ownership; presenting both is not a useful
    /// second application.
    pub duplicate_aliases: usize,
    /// Removed by a `Hidden=true` tombstone.
    pub tombstoned: usize,
    /// `Type=` was not `Application`.
    pub not_an_application: usize,
    /// `NoDisplay=true`.
    pub no_display: usize,
    /// Excluded by `OnlyShowIn` / `NotShowIn`.
    pub other_desktop: usize,
    /// `TryExec=` named a program that is not on this system.
    pub try_exec_missing: usize,
    /// `Type=Application` with no `Exec=`, so nothing to run.
    pub no_exec: usize,
}

/// The result of one scan: the launchable entries, sorted by name, plus the accounting.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub entries: Vec<DesktopEntry>,
    pub stats: ScanStats,
    /// The directories that were searched, in precedence order. Kept so a cache can stat them
    /// for changes and a diagnostic can print them.
    pub dirs: Vec<PathBuf>,
    /// The IDs a `Hidden=true` entry removed, sorted: what the person took off their apps.
    pub hidden: Vec<String>,
}

impl Scan {
    pub fn find(&self, id: &str) -> Option<&DesktopEntry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// The `applications` directories to search, in precedence order. Reads the environment, so
/// it reflects the session the compositor actually started in rather than a compiled-in guess.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    if let Some(home) = tilo_paths::xdg::data_home() {
        push(home.join("applications"));
    }
    for base in tilo_paths::xdg::data_dirs() {
        push(base.join("applications"));
    }
    // Flatpak's exports. Appended, not prepended: the dedup above keeps their real precedence if
    // the session already listed them, and appending is correct otherwise — a hand-installed
    // entry should override an export.
    if let Some(home) = tilo_paths::home() {
        push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs
}

/// Scan the XDG application directories of the current environment.
pub fn scan() -> Scan {
    scan_dirs(&application_dirs(), &current_desktops())
}

/// Scan an explicit list of `applications` directories, in precedence order, filtering for
/// `desktop`. Split out from [`scan`] so tests drive real directories they built, rather
/// than whatever the machine running them happens to have installed.
pub fn scan_dirs(dirs: &[PathBuf], desktop: &[String]) -> Scan {
    let locales = locale_candidates();
    let mut stats = ScanStats::default();
    // id -> entry, first writer wins.
    let mut claimed: BTreeMap<String, DesktopEntry> = BTreeMap::new();
    // IDs a tombstone has removed. Recorded separately so a later directory's file for the same
    // ID stays suppressed.
    let mut tombstoned: Vec<String> = Vec::new();

    for dir in dirs {
        for (id, path) in entry_files(dir) {
            stats.files_seen = stats.files_seen.saturating_add(1);
            if claimed.contains_key(&id) || tombstoned.contains(&id) {
                stats.shadowed = stats.shadowed.saturating_add(1);
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                stats.unparseable = stats.unparseable.saturating_add(1);
                continue;
            };
            let Some(entry) = parse(&contents, &id, &path, &locales) else {
                stats.unparseable = stats.unparseable.saturating_add(1);
                continue;
            };
            let mut entry = entry;
            if let Some(downloaded) = entry.downloaded.as_mut() {
                // The scan runs at startup and on its worker, never on the
                // compositor's loop: this is where a missing file is noticed.
                downloaded.present = std::fs::metadata(&downloaded.program).is_ok();
            }
            if entry.hidden {
                stats.tombstoned = stats.tombstoned.saturating_add(1);
                tombstoned.push(id);
                continue;
            }
            claimed.insert(id, entry);
        }
    }

    let mut entries: Vec<DesktopEntry> = Vec::with_capacity(claimed.len());
    for entry in claimed.into_values() {
        if entry.entry_type != EntryType::Application {
            stats.not_an_application = stats.not_an_application.saturating_add(1);
            continue;
        }
        if entry.exec.is_none() {
            stats.no_exec = stats.no_exec.saturating_add(1);
            continue;
        }
        if entry.no_display {
            stats.no_display = stats.no_display.saturating_add(1);
            continue;
        }
        if !entry.shows_in(desktop) {
            stats.other_desktop = stats.other_desktop.saturating_add(1);
            continue;
        }
        if let Some(try_exec) = entry.try_exec.as_deref()
            && !program_exists(try_exec)
        {
            stats.try_exec_missing = stats.try_exec_missing.saturating_add(1);
            continue;
        }
        if let Some(existing_index) = entries
            .iter()
            .position(|existing| desktop_entries_are_aliases(existing, &entry))
        {
            stats.duplicate_aliases = stats.duplicate_aliases.saturating_add(1);
            // Where a host package and an identically identified Flatpak are
            // both installed, retain the Flatpak. It has its own confinement,
            // and this matches the host-app catalogue's existing preference.
            if let Some(existing) = entries.get_mut(existing_index)
                && entry_is_flatpak(&entry)
                && !entry_is_flatpak(existing)
            {
                *existing = entry;
            }
        } else {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });

    tombstoned.sort();
    Scan {
        entries,
        stats,
        dirs: dirs.to_vec(),
        hidden: tombstoned,
    }
}

/// Two separate desktop-file ids may be aliases, but name equality alone is
/// far too broad ("Settings" and "Terminal" are common independent apps).
/// The raw-command case is exact and source-agnostic.  A host/Flatpak pair
/// with different launcher commands needs a stronger, stable identity: both
/// files must declare the same `StartupWMClass`, and it must be exactly the
/// Flatpak application id.  That is the freedesktop identity a compositor
/// later uses to associate the mapped window with its launcher entry.
fn desktop_entries_are_aliases(existing: &DesktopEntry, candidate: &DesktopEntry) -> bool {
    if !existing.name.eq_ignore_ascii_case(&candidate.name) {
        return false;
    }
    if existing.exec == candidate.exec {
        return true;
    }
    let (flatpak, host) = match (&existing.source, &candidate.source) {
        (EntrySource::Flatpak { .. }, EntrySource::Host) => (existing, candidate),
        (EntrySource::Host, EntrySource::Flatpak { .. }) => (candidate, existing),
        _ => return false,
    };
    let EntrySource::Flatpak { app_id } = &flatpak.source else {
        return false;
    };
    flatpak
        .startup_wm_class
        .as_deref()
        .zip(host.startup_wm_class.as_deref())
        .is_some_and(|(flatpak_class, host_class)| flatpak_class == app_id && host_class == app_id)
}

fn entry_is_flatpak(entry: &DesktopEntry) -> bool {
    matches!(entry.source, EntrySource::Flatpak { .. })
}

/// Every `.desktop` file under `dir`, with its desktop-file ID. The spec makes subdirectories
/// part of the ID (`kde4/konsole.desktop` → `kde4-konsole`), so this walks rather than lists;
/// depth is capped since it's the only thing depth buys and a symlink loop isn't worth the risk.
fn entry_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

const MAX_DEPTH: usize = 6;

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // `metadata()` follows symlinks, which is what we want: Flatpak and distribution packaging
        // both export entries as symlinks.
        let is_dir = if file_type.is_symlink() {
            std::fs::metadata(&path).is_ok_and(|m| m.is_dir())
        } else {
            file_type.is_dir()
        };
        if is_dir {
            walk(root, &path, depth.saturating_add(1), out);
            continue;
        }
        if path.extension().and_then(OsStr::to_str) != Some("desktop") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let id = relative
            .to_string_lossy()
            .trim_end_matches(".desktop")
            .replace('/', "-");
        if id.is_empty() {
            continue;
        }
        out.push((id, path));
    }
}

/// Whether `TryExec` names something runnable. An absolute path is checked directly; a bare
/// name is looked up on `PATH`.
pub fn program_exists(program: &str) -> bool {
    let program = program.trim();
    if program.is_empty() {
        return false;
    }
    let candidate = Path::new(program);
    if candidate.is_absolute() || program.contains('/') {
        return is_executable(candidate);
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    path.to_string_lossy()
        .split(':')
        .filter(|s| !s.is_empty())
        .any(|dir| is_executable(&Path::new(dir).join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntrySource;

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn app(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/bin/true\n")
    }

    fn tilo() -> Vec<String> {
        vec![CURRENT_DESKTOP.to_string()]
    }

    #[test]
    fn scanned_ids_come_from_the_first_directory_and_tombstones_remove_them() {
        // Phase `a_subdirectory_becomes_part_of_the_id`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(tmp.path(), "kde4/konsole.desktop", &app("Konsole"));
            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 1);
            assert_eq!(scan.entries[0].id, "kde4-konsole");
        }
        // Phase `the_first_directory_to_provide_an_id_wins`.
        // The whole point of the precedence order: a user's file replaces the system's, and the
        // system's copy is not offered twice.
        {
            let user = tempfile::tempdir().unwrap();
            let system = tempfile::tempdir().unwrap();
            write(user.path(), "x.desktop", &app("Mine"));
            write(system.path(), "x.desktop", &app("Theirs"));
            let scan = scan_dirs(
                &[user.path().to_path_buf(), system.path().to_path_buf()],
                &tilo(),
            );
            assert_eq!(
                scan.entries.len(),
                1,
                "one id, one entry: {:?}",
                scan.entries
            );
            assert_eq!(scan.entries[0].name, "Mine");
            assert_eq!(scan.stats.shadowed, 1);
        }
        // Phase `a_hidden_tombstone_removes_the_id_from_every_directory`.
        {
            let user = tempfile::tempdir().unwrap();
            let system = tempfile::tempdir().unwrap();
            write(
                user.path(),
                "x.desktop",
                "[Desktop Entry]\nType=Application\nName=X\nExec=/bin/true\nHidden=true\n",
            );
            write(
                user.path(),
                "so.tilo.notes.desktop",
                "[Desktop Entry]\nType=Application\nName=Notes\nHidden=true\n",
            );
            write(system.path(), "x.desktop", &app("X"));
            let scan = scan_dirs(
                &[user.path().to_path_buf(), system.path().to_path_buf()],
                &tilo(),
            );
            assert!(scan.entries.is_empty(), "{:?}", scan.entries);
            assert_eq!(scan.stats.tombstoned, 2);
            assert_eq!(scan.hidden, ["so.tilo.notes", "x"]);
        }
        // Phase `a_missing_directory_is_not_an_error`.
        {
            let scan = scan_dirs(&[PathBuf::from("/nonexistent/tilo/applications")], &tilo());
            assert!(scan.entries.is_empty());
            assert_eq!(scan.stats.files_seen, 0);
        }
        // Phase `application_dirs_always_include_the_flatpak_exports`.
        {
            let dirs = application_dirs();
            assert!(
                dirs.iter()
                    .any(|d| d.ends_with("var/lib/flatpak/exports/share/applications")),
                "system Flatpak exports must be searched even when \
                 XDG_DATA_DIRS does not name them: {dirs:?}"
            );
        }
    }

    #[test]
    fn scanned_rows_merge_by_stable_identity_but_keep_distinct_programs() {
        // Phase `differently_named_files_for_the_same_application_are_one_row`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let body = "[Desktop Entry]\nType=Application\nName=Chat\n\
                        Exec=/usr/bin/chat %U\nStartupWMClass=Chat\n";
            write(tmp.path(), "vendor-chat.desktop", body);
            write(tmp.path(), "chat.desktop", body);

            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 1, "aliases must not become two apps");
            assert_eq!(scan.stats.duplicate_aliases, 1);
        }
        // Phase `host_and_flatpak_exports_with_the_same_stable_identity_are_one_row`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(
                tmp.path(),
                "chat-system.desktop",
                "[Desktop Entry]\nType=Application\nName=Chat\n\
                 Exec=/usr/bin/chat\nStartupWMClass=org.example.Chat\n",
            );
            write(
                tmp.path(),
                "org.example.Chat.desktop",
                "[Desktop Entry]\nType=Application\nName=Chat\n\
                 Exec=/usr/bin/flatpak run org.example.Chat\n\
                 X-Flatpak=org.example.Chat\nStartupWMClass=org.example.Chat\n",
            );

            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 1, "one application must have one row");
            assert_eq!(scan.stats.duplicate_aliases, 1);
            assert!(matches!(
                scan.entries[0].source,
                EntrySource::Flatpak { .. }
            ));
        }
        // Phase `same_name_host_and_flatpak_entries_without_a_shared_stable_identity_remain_distinct`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(
                tmp.path(),
                "chat-system.desktop",
                "[Desktop Entry]\nType=Application\nName=Chat\n\
                 Exec=/usr/bin/chat\nStartupWMClass=chat-system\n",
            );
            write(
                tmp.path(),
                "org.example.Chat.desktop",
                "[Desktop Entry]\nType=Application\nName=Chat\n\
                 Exec=/usr/bin/flatpak run org.example.Chat\n\
                 X-Flatpak=org.example.Chat\nStartupWMClass=org.example.Chat\n",
            );

            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 2, "names alone must never merge apps");
            assert_eq!(scan.stats.duplicate_aliases, 0);
        }
        // Phase `distinct_modes_of_one_program_remain_distinct_apps`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(tmp.path(), "editor.desktop", &app("Editor"));
            write(
                tmp.path(),
                "editor-safe.desktop",
                "[Desktop Entry]\nType=Application\nName=Editor\nExec=/bin/true --safe\n",
            );

            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 2);
            assert_eq!(scan.stats.duplicate_aliases, 0);
        }
    }

    #[test]
    fn scanned_entries_are_filtered_counted_sorted_and_labelled() {
        // Phase `nodisplay_and_wrong_type_and_missing_exec_are_each_counted`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(
                tmp.path(),
                "a.desktop",
                "[Desktop Entry]\nType=Application\nName=A\nExec=/bin/true\nNoDisplay=true\n",
            );
            write(
                tmp.path(),
                "b.desktop",
                "[Desktop Entry]\nType=Link\nName=B\nURL=https://x\n",
            );
            write(
                tmp.path(),
                "c.desktop",
                "[Desktop Entry]\nType=Application\nName=C\n",
            );
            write(tmp.path(), "d.desktop", &app("D"));
            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 1);
            assert_eq!(scan.entries[0].name, "D");
            assert_eq!(scan.stats.no_display, 1);
            assert_eq!(scan.stats.not_an_application, 1);
            assert_eq!(scan.stats.no_exec, 1);
            assert_eq!(scan.stats.files_seen, 4);
        }
        // Phase `entries_come_back_sorted_by_name`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(tmp.path(), "z.desktop", &app("alpha"));
            write(tmp.path(), "a.desktop", &app("Zulu"));
            write(tmp.path(), "m.desktop", &app("Mike"));
            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            let names: Vec<&str> = scan.entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["alpha", "Mike", "Zulu"]);
        }
        // Phase `a_flatpak_export_is_labelled_as_one`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(
                tmp.path(),
                "com.x.App.desktop",
                "[Desktop Entry]\nType=Application\nName=App\n\
                 Exec=/usr/bin/flatpak run com.x.App\nX-Flatpak=com.x.App\n",
            );
            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(
                scan.entries[0].source,
                EntrySource::Flatpak {
                    app_id: "com.x.App".into()
                }
            );
        }
        // Phase `try_exec_that_does_not_resolve_drops_the_entry`.
        {
            let tmp = tempfile::tempdir().unwrap();
            write(
                tmp.path(),
                "gone.desktop",
                "[Desktop Entry]\nType=Application\nName=Gone\nExec=/bin/true\n\
                 TryExec=/nonexistent/definitely-not-here\n",
            );
            write(
                tmp.path(),
                "here.desktop",
                "[Desktop Entry]\nType=Application\nName=Here\nExec=/bin/true\nTryExec=/bin/true\n",
            );
            let scan = scan_dirs(&[tmp.path().to_path_buf()], &tilo());
            assert_eq!(scan.entries.len(), 1);
            assert_eq!(scan.entries[0].name, "Here");
            assert_eq!(scan.stats.try_exec_missing, 1);
        }
    }
}
