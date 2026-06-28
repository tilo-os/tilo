//! Wayland session discovery.
//!
//! Reads `*.desktop` files out of a sessions directory (in production
//! `/usr/share/wayland-sessions`) and extracts the human-readable `Name=`
//! and the `Exec=` command line. `Exec` is split on ASCII whitespace into
//! an argv vector suitable for greetd's `StartSession { cmd, env }`.
//!
//! This is deliberately a *minimal* desktop-entry parser: we only need
//! `Name` and `Exec` from the `[Desktop Entry]` group, and the session
//! files we ship are simple (no `%`-field codes, no quoting). It ignores
//! localized `Name[xx]=` keys and keeps the first unlocalized `Name=`.

use std::path::Path;

/// One launchable Wayland session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Human-readable label shown on the session-cycle button.
    pub name: String,
    /// `Exec=` split on whitespace — argv[0] is the program.
    pub argv: Vec<String>,
}

/// Parse a single `.desktop` file's contents into a [`Session`]. Returns
/// `None` if it lacks a usable `Exec=` line (a session with no command is
/// meaningless). A missing `Name=` falls back to argv[0]'s basename.
pub fn parse_desktop_entry(contents: &str) -> Option<Session> {
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Group headers like `[Desktop Entry]` / `[Desktop Action ...]`.
        // We only care about the first (unlocalized) Name/Exec we see,
        // which in our shipped files live in `[Desktop Entry]`.
        if line.starts_with('[') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(rest.trim().to_string());
            }
        } else if let Some(rest) = line.strip_prefix("Exec=") {
            if exec.is_none() {
                exec = Some(rest.trim().to_string());
            }
        }
        // Localized `Name[de]=` etc. fall through (don't match `Name=`).
    }

    let exec = exec?;
    let argv: Vec<String> = exec.split_whitespace().map(|s| s.to_string()).collect();
    if argv.is_empty() {
        return None;
    }

    let name = name.unwrap_or_else(|| {
        Path::new(&argv[0])
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&argv[0])
            .to_string()
    });

    Some(Session { name, argv })
}

/// Read every `*.desktop` entry in `dir`, parse it, and return the
/// sessions sorted by name for a stable cycle order. A missing or
/// unreadable directory yields an empty vec (the caller surfaces that as
/// "no session available").
pub fn read_sessions(dir: &Path) -> Vec<Session> {
    let mut sessions = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Some(session) = parse_desktop_entry(&contents) {
                sessions.push(session);
            }
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

    // The two real entries shipped in os/usr/share/wayland-sessions/.
    const NIRIOS: &str = "\
[Desktop Entry]
Name=nirios
GenericName=qOS
Comment=Collaborative spatial desktop environment
Exec=/usr/bin/shell --backend drm
Type=Application
DesktopNames=nirios
";

    const NIRIOS_PEER_SYNC: &str = "\
[Desktop Entry]
Name=nirios (peer-sync)
GenericName=qOS with peer-sync
Comment=Collaborative session with libp2p peer discovery enabled
Exec=/usr/bin/shell --backend drm --enable-peer-sync
Type=Application
DesktopNames=nirios
";

    #[test]
    fn parses_default_nirios_session() {
        let s = parse_desktop_entry(NIRIOS).expect("should parse");
        assert_eq!(s.name, "nirios");
        assert_eq!(s.argv, vec!["/usr/bin/shell", "--backend", "drm"]);
    }

    #[test]
    fn parses_peer_sync_session() {
        let s = parse_desktop_entry(NIRIOS_PEER_SYNC).expect("should parse");
        assert_eq!(s.name, "nirios (peer-sync)");
        assert_eq!(
            s.argv,
            vec!["/usr/bin/shell", "--backend", "drm", "--enable-peer-sync"]
        );
    }

    #[test]
    fn comments_and_localized_names_are_ignored() {
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

    #[test]
    fn missing_exec_yields_none() {
        let entry = "[Desktop Entry]\nName=No Exec\nType=Application\n";
        assert!(parse_desktop_entry(entry).is_none());
    }

    #[test]
    fn read_sessions_reads_a_directory() {
        let dir = std::env::temp_dir().join(format!("qos-greeter-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.desktop"), NIRIOS).unwrap();
        std::fs::write(dir.join("b.desktop"), NIRIOS_PEER_SYNC).unwrap();
        // A non-.desktop file must be skipped.
        std::fs::write(dir.join("README.txt"), "ignore me").unwrap();

        let sessions = read_sessions(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(sessions.len(), 2);
        // Sorted by name: "nirios" before "nirios (peer-sync)".
        assert_eq!(sessions[0].name, "nirios");
        assert_eq!(sessions[1].name, "nirios (peer-sync)");
    }

    #[test]
    fn missing_directory_is_empty() {
        let sessions = read_sessions(Path::new("/nonexistent/qos/sessions/dir"));
        assert!(sessions.is_empty());
    }
}
