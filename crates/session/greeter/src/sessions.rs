//! Wayland session discovery.
//!
//! Reads `*.desktop` files out of a sessions directory (in production
//! `/usr/share/wayland-sessions`) and extracts the human-readable `Name=`
//! and the `Exec=` command line. `Exec` is split on ASCII whitespace into
//! an argv vector suitable for greetd's `StartSession { cmd, env }`.
//!
//! `Name` is the label the greeter shows. The internal `DesktopNames=` id
//! is not read for display: it is a machine-facing contract
//! with the portal (`UseIn=tilo`), not something to put in front of
//! someone logging in.
//!
//! This is a *minimal* desktop-entry parser: it reads `Name`, `Exec`,
//! `TryExec`, `Hidden` and `NoDisplay` from the `[Desktop Entry]` group, and
//! the session files we ship are simple (no `%`-field codes, no quoting). It
//! ignores localized `Name[xx]=` keys and keeps the first unlocalized `Name=`.
//!
//! Only real, launchable sessions count: an entry that hides itself, or whose
//! `TryExec` program is not installed, is not offered. The greeter shows a
//! session choice only when more than one remains.

use std::path::Path;

/// One launchable Wayland session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Human-readable label shown in the session picker.
    pub name: String,
    /// `Exec=` split on whitespace — argv`0` is the program.
    pub argv: Vec<String>,
}

/// A parsed entry before the installed-program check.
struct Entry {
    session: Session,
    try_exec: Option<String>,
}

fn parse_entry(contents: &str) -> Option<Entry> {
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut try_exec: Option<String> = None;
    let mut in_entry = false;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Keys belong to the group above them; only `[Desktop Entry]`
        // describes the session itself (`[Desktop Action ...]` does not).
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" if name.is_none() => name = Some(value.to_string()),
            "Exec" if exec.is_none() => exec = Some(value.to_string()),
            "TryExec" if try_exec.is_none() => try_exec = Some(value.to_string()),
            "Hidden" | "NoDisplay" if value == "true" => return None,
            // Localized `Name[de]=` etc. fall through (don't match `Name`).
            _ => {}
        }
    }

    let exec = exec?;
    let argv: Vec<String> = exec
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect();
    let first = argv.first()?.clone();
    let name = name.filter(|name| !name.is_empty()).unwrap_or_else(|| {
        Path::new(&first)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&first)
            .to_string()
    });

    Some(Entry {
        session: Session { name, argv },
        try_exec: try_exec.filter(|program| !program.is_empty()),
    })
}

/// Parse a single `.desktop` file's contents into a [`Session`]. Returns
/// `None` if it lacks a usable `Exec=` line (a session with no command is
/// meaningless) or hides itself with `Hidden=true` / `NoDisplay=true`. A
/// missing `Name=` falls back to argv`0`'s basename.
pub fn parse_desktop_entry(contents: &str) -> Option<Session> {
    parse_entry(contents).map(|entry| entry.session)
}

/// Whether `program` (a path, or a name looked up on `path`) exists.
fn installed(program: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let program = Path::new(program);
    if program.is_absolute() {
        return program.is_file();
    }
    path.is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join(program).is_file()))
}

/// Read every `*.desktop` entry in `dir`, parse it, and return the
/// launchable sessions sorted by name for a stable order. Entries whose
/// `TryExec` program is missing, and duplicates of a command already
/// listed, are dropped. A missing or unreadable directory yields an empty
/// vec (the caller surfaces that as "no session available").
pub fn read_sessions(dir: &Path) -> Vec<Session> {
    read_sessions_with_path(dir, std::env::var_os("PATH").as_deref())
}

fn read_sessions_with_path(dir: &Path, path: Option<&std::ffi::OsStr>) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sessions;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for file in paths {
        if file.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Some(entry) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|contents| parse_entry(&contents))
        else {
            continue;
        };
        if entry
            .try_exec
            .as_deref()
            .is_some_and(|program| !installed(program, path))
        {
            continue;
        }
        if sessions
            .iter()
            .all(|known| known.argv != entry.session.argv)
        {
            sessions.push(entry.session);
        }
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    sessions
}

/// The standard system location for Wayland session entries.
pub const DEFAULT_SESSIONS_DIR: &str = "/usr/share/wayland-sessions";

#[cfg(test)]
mod tests {
    use super::*;

    // The entry shipped in os/usr/share/wayland-sessions/.
    const TILO: &str = "\
[Desktop Entry]
Name=Tilo
GenericName=Desktop
Comment=Collaborative spatial desktop environment
Exec=/usr/libexec/tilo-session --backend drm
Type=Application
DesktopNames=tilo
";

    // A second session someone installed alongside.
    const WESTON: &str = "\
[Desktop Entry]
Name=Weston
Comment=The reference Wayland compositor
Exec=/usr/bin/weston
Type=Application
";

    #[test]
    fn session_desktop_files_are_discovered_and_parsed() {
        // Phase `parses_default_tilo_session`.
        {
            let s = parse_desktop_entry(TILO).expect("should parse");
            assert_eq!(s.name, "Tilo");
            assert_eq!(
                s.argv,
                vec!["/usr/libexec/tilo-session", "--backend", "drm"]
            );
        }
        // Phase `parses_another_session`.
        {
            let s = parse_desktop_entry(WESTON).expect("should parse");
            assert_eq!(s.name, "Weston");
            assert_eq!(s.argv, vec!["/usr/bin/weston"]);
        }
        // Phase `comments_and_localized_names_are_ignored`.
        {
            let entry = "\
    [Desktop Entry]
    # a comment
    Name=Real Name
    Name[de]=Echter Name
    Exec=/usr/bin/foo --bar
    ";
            let s = parse_desktop_entry(entry).expect("should parse");
            assert_eq!(s.name, "Real Name");
            assert_eq!(s.argv, vec!["/usr/bin/foo", "--bar"]);
        }
        // Phase `missing_exec_yields_none`.
        {
            let entry = "[Desktop Entry]\nName=No Exec\nType=Application\n";
            assert!(parse_desktop_entry(entry).is_none());
        }
        // Phase `read_sessions_reads_a_directory`.
        {
            let dir =
                std::env::temp_dir().join(format!("tilo-greeter-sessions-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a.desktop"), TILO).unwrap();
            std::fs::write(dir.join("b.desktop"), WESTON).unwrap();
            // A non-.desktop file must be skipped.
            std::fs::write(dir.join("README.txt"), "ignore me").unwrap();

            let sessions = read_sessions(&dir);
            let _ = std::fs::remove_dir_all(&dir);

            assert_eq!(sessions.len(), 2);
            // Sorted by name: "Tilo" before "Weston".
            assert_eq!(sessions[0].name, "Tilo");
            assert_eq!(sessions[1].name, "Weston");
        }
        // Phase `missing_directory_is_empty`.
        {
            let sessions = read_sessions(Path::new("/nonexistent/tilo/sessions/dir"));
            assert!(sessions.is_empty());
        }
    }

    #[test]
    fn only_real_launchable_sessions_are_offered() {
        let dir =
            std::env::temp_dir().join(format!("tilo-greeter-real-sessions-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("weston"), "").unwrap();
        let sessions_dir = dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("tilo.desktop"), TILO).unwrap();
        // The same command under another file is one session, not two.
        std::fs::write(sessions_dir.join("tilo-copy.desktop"), TILO).unwrap();
        std::fs::write(
            sessions_dir.join("hidden.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=/usr/bin/hidden\nHidden=true\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("nodisplay.desktop"),
            "[Desktop Entry]\nName=Quiet\nExec=/usr/bin/quiet\nNoDisplay=true\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("missing.desktop"),
            "[Desktop Entry]\nName=Gone\nTryExec=not-installed-anywhere\nExec=gone\n",
        )
        .unwrap();
        // An action group's keys do not describe the session.
        std::fs::write(
            sessions_dir.join("action.desktop"),
            "[Desktop Action other]\nName=Other\nExec=/usr/bin/other\n",
        )
        .unwrap();

        let path = std::ffi::OsString::from(bin.as_os_str());
        let one = read_sessions_with_path(&sessions_dir, Some(&path));
        let names: Vec<&str> = one.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Tilo"]);

        std::fs::write(
            sessions_dir.join("weston.desktop"),
            "[Desktop Entry]\nName=Weston\nTryExec=weston\nExec=weston\n",
        )
        .unwrap();
        let two = read_sessions_with_path(&sessions_dir, Some(&path));
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<&str> = two.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Tilo", "Weston"]);
    }
}
