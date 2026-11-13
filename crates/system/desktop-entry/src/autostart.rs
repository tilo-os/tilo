//! XDG autostart: the `.desktop` files a session starts at login, from
//! `$XDG_CONFIG_HOME/autostart` and then each `$XDG_CONFIG_DIRS/autostart`.
//! A file name is the identity: the first directory holding `foo.desktop`
//! decides it, so a user file overrides (or, with `Hidden=true`, cancels) the
//! system one.
//!
//! Tilo applies the specification's rules plus the GNOME extensions the way
//! `gnome-session` does. [`AUTOSTART_DESKTOPS`] and [`DESKTOP_PHASES`] name
//! the entries a Tilo session leaves out, because it provides that part of the
//! desktop itself or systemd activates it on demand.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::entry::{DesktopEntry, EntryType, parse, unlocalised_groups};

/// The desktop names an entry's `OnlyShowIn`/`NotShowIn` are matched
/// against: Tilo's own name only. `XDG_CURRENT_DESKTOP` also carries `GNOME`
/// so GNOME-only applications stay in the menu, but a GNOME-only autostart
/// entry is a piece of the GNOME session and does not belong to Tilo's.
pub const AUTOSTART_DESKTOPS: &[&str] = &["tilo"];

pub const SESSION_NAME: &str = "tilo";

/// `X-GNOME-Autostart-Phase` values naming a part of the desktop Tilo
/// provides itself, or infrastructure systemd activates on demand.
const DESKTOP_PHASES: &[&str] = &[
    "PreDisplayServer",
    "DisplayServer",
    "WindowManager",
    "Panel",
    "Desktop",
];

pub fn autostart_dirs() -> Vec<PathBuf> {
    autostart_dirs_from(
        tilo_paths::xdg::config_home().as_deref(),
        &tilo_paths::xdg::config_dirs(),
    )
}

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

/// Why an autostart file is not started, so a session log can explain a
/// program that did not come up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    NotAnApplication,
    Hidden,
    NotShownIn,
    Disabled,
    TryExecMissing(String),
    NoExec,
    /// `X-GNOME-HiddenUnderSystemd=true`: a systemd user unit runs it instead.
    HiddenUnderSystemd,
    /// `X-GNOME-Autostart-Phase` is a part of the desktop Tilo provides itself.
    DesktopPhase(String),
    ConditionFalse(String),
    UnknownCondition(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    IfExists(String),
    UnlessExists(String),
    GSettings { schema: String, key: String },
    IfSession(String),
    UnlessSession(String),
    Unknown,
}

impl Condition {
    pub fn parse(value: &str) -> Self {
        let mut words = value.split_whitespace();
        let kind = words.next().unwrap_or_default();
        let rest: Vec<&str> = words.collect();
        match (kind, rest.as_slice()) {
            ("if-exists", [_, ..]) => Self::IfExists(rest.join(" ")),
            ("unless-exists", [_, ..]) => Self::UnlessExists(rest.join(" ")),
            ("GSettings", [schema, key]) => Self::GSettings {
                schema: (*schema).to_string(),
                key: (*key).to_string(),
            },
            ("GNOME3", ["if-session", name]) => Self::IfSession((*name).to_string()),
            ("GNOME3", ["unless-session", name]) => Self::UnlessSession((*name).to_string()),
            _ => Self::Unknown,
        }
    }

    pub fn holds(&self, probe: &impl AutostartProbe) -> Option<bool> {
        Some(match self {
            Self::IfExists(file) => probe.config_file_exists(file),
            Self::UnlessExists(file) => !probe.config_file_exists(file),
            // A missing schema or key is a false condition, as in gnome-session.
            Self::GSettings { schema, key } => probe.gsettings_bool(schema, key).unwrap_or(false),
            Self::IfSession(name) => name == SESSION_NAME,
            Self::UnlessSession(name) => name != SESSION_NAME,
            Self::Unknown => return None,
        })
    }
}

/// What deciding an autostart entry needs to know about the machine; tests
/// answer from fixtures, the session from the real system.
pub trait AutostartProbe {
    fn program_exists(&self, program: &str) -> bool;
    fn gsettings_bool(&self, schema: &str, key: &str) -> Option<bool>;
    fn config_file_exists(&self, relative: &str) -> bool;
}

pub fn config_file_exists(relative: &str) -> bool {
    tilo_paths::xdg::config_home()
        .is_some_and(|home| home.join(relative.trim_start_matches('/')).exists())
}

#[derive(Debug, Clone)]
pub struct Autostart {
    pub entry: DesktopEntry,
    /// `X-GNOME-AutoRestart=true`: restart the program when it dies.
    pub auto_restart: bool,
}

#[derive(Debug, Default, Clone)]
pub struct AutostartScan {
    pub start: Vec<Autostart>,
    pub skipped: Vec<(PathBuf, Skipped)>,
}

pub fn scan_autostart(
    dirs: &[PathBuf],
    probe: &impl AutostartProbe,
    locales: &[String],
) -> AutostartScan {
    let desktops: Vec<String> = AUTOSTART_DESKTOPS
        .iter()
        .map(|desktop| (*desktop).to_string())
        .collect();
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
            let Some(entry) = parse(&text, id, &path, locales) else {
                continue;
            };
            let keys = AutostartKeys::read(&text);
            match why_skipped(&entry, &keys, &desktops, probe) {
                Some(reason) => out.skipped.push((path, reason)),
                None => out.start.push(Autostart {
                    entry,
                    auto_restart: keys.auto_restart,
                }),
            }
        }
    }
    out
}

/// The GNOME autostart keys, which only matter here and so are not part of
/// [`DesktopEntry`].
#[derive(Debug, Default)]
struct AutostartKeys {
    condition: Option<String>,
    phase: Option<String>,
    hidden_under_systemd: bool,
    auto_restart: bool,
}

impl AutostartKeys {
    fn read(text: &str) -> Self {
        let groups = unlocalised_groups(text);
        let Some(main) = groups.get("Desktop Entry") else {
            return Self::default();
        };
        let value = |key: &str| {
            main.get(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let flag = |key: &str| value(key).is_some_and(|value| value == "true");
        Self {
            condition: value("AutostartCondition").or_else(|| value("X-GNOME-AutostartCondition")),
            phase: value("X-GNOME-Autostart-Phase"),
            hidden_under_systemd: flag("X-GNOME-HiddenUnderSystemd"),
            auto_restart: flag("X-GNOME-AutoRestart"),
        }
    }
}

fn why_skipped(
    entry: &DesktopEntry,
    keys: &AutostartKeys,
    desktops: &[String],
    probe: &impl AutostartProbe,
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
    if keys.hidden_under_systemd {
        return Some(Skipped::HiddenUnderSystemd);
    }
    if let Some(phase) = keys.phase.as_deref()
        && DESKTOP_PHASES.contains(&phase)
    {
        return Some(Skipped::DesktopPhase(phase.to_string()));
    }
    if let Some(program) = entry.try_exec.as_deref()
        && !probe.program_exists(program)
    {
        return Some(Skipped::TryExecMissing(program.to_string()));
    }
    if entry.exec.is_none() {
        return Some(Skipped::NoExec);
    }
    // Last, since a GSettings condition asks the system.
    if let Some(condition) = keys.condition.as_deref() {
        match Condition::parse(condition).holds(probe) {
            Some(true) => {}
            Some(false) => return Some(Skipped::ConditionFalse(condition.to_string())),
            None => return Some(Skipped::UnknownCondition(condition.to_string())),
        }
    }
    None
}

/// `TryExec` per the specification: an absolute path must be an executable
/// file; a bare name is looked up on `PATH`.
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
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct Probe {
        programs: Vec<&'static str>,
        settings: BTreeMap<(&'static str, &'static str), bool>,
        config_files: Vec<&'static str>,
    }

    impl AutostartProbe for Probe {
        fn program_exists(&self, program: &str) -> bool {
            self.programs.contains(&program)
        }
        fn gsettings_bool(&self, schema: &str, key: &str) -> Option<bool> {
            self.settings
                .iter()
                .find(|((s, k), _)| *s == schema && *k == key)
                .map(|(_, value)| *value)
        }
        fn config_file_exists(&self, relative: &str) -> bool {
            self.config_files.contains(&relative)
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn app(extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=App\nExec=app\n{extra}")
    }

    fn reason(scan: &AutostartScan, id: &str) -> Option<Skipped> {
        scan.skipped
            .iter()
            .find(|(path, _)| path.file_stem().is_some_and(|stem| stem == id))
            .map(|(_, reason)| reason.clone())
    }

    fn started(scan: &AutostartScan) -> Vec<&str> {
        scan.start
            .iter()
            .map(|start| start.entry.id.as_str())
            .collect()
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
        write(
            &system,
            "reenabled.desktop",
            &app("X-GNOME-Autostart-enabled=false\n"),
        );
        write(&user, "reenabled.desktop", &app(""));
        write(&system, "plain.desktop", &app(""));
        write(&system, "gnome-only.desktop", &app("OnlyShowIn=GNOME;\n"));
        write(&system, "not-gnome.desktop", &app("NotShowIn=GNOME;\n"));
        write(&system, "not-tilo.desktop", &app("NotShowIn=Tilo;\n"));
        write(
            &system,
            "tilo-only.desktop",
            &app("OnlyShowIn=GNOME;tilo;\n"),
        );
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
            "unit.desktop",
            &app("X-GNOME-HiddenUnderSystemd=true\n"),
        );
        write(
            &system,
            "panel.desktop",
            &app("X-GNOME-Autostart-Phase=Panel\n"),
        );
        write(
            &system,
            "init.desktop",
            &app("X-GNOME-Autostart-Phase=Initialization\nX-systemd-skip=true\n"),
        );
        write(
            &system,
            "restarts.desktop",
            &app("X-GNOME-AutoRestart=true\n"),
        );
        write(
            &system,
            "link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=x\n",
        );
        write(&system, "readme.txt", "not a desktop file");

        let probe = Probe {
            programs: vec!["present"],
            ..Probe::default()
        };
        let scan = scan_autostart(&[user, system], &probe, &[]);
        assert_eq!(
            started(&scan),
            vec![
                "reenabled",
                "found",
                "init",
                "not-gnome",
                "plain",
                "restarts",
                "tilo-only"
            ]
        );
        assert!(
            scan.start
                .iter()
                .all(|start| start.auto_restart == (start.entry.id == "restarts"))
        );
        assert_eq!(reason(&scan, "overridden"), Some(Skipped::Hidden));
        assert_eq!(reason(&scan, "gnome-only"), Some(Skipped::NotShownIn));
        assert_eq!(reason(&scan, "not-tilo"), Some(Skipped::NotShownIn));
        assert_eq!(reason(&scan, "off"), Some(Skipped::Disabled));
        assert_eq!(
            reason(&scan, "tryexec"),
            Some(Skipped::TryExecMissing("/nonexistent/helper".into()))
        );
        assert_eq!(reason(&scan, "unit"), Some(Skipped::HiddenUnderSystemd));
        assert_eq!(
            reason(&scan, "panel"),
            Some(Skipped::DesktopPhase("Panel".into()))
        );
        assert_eq!(reason(&scan, "link"), Some(Skipped::NotAnApplication));
    }

    #[test]
    fn autostart_conditions_follow_gnome_session() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("autostart");
        let cases = [
            ("setting-on", "GSettings org.example.app enabled", true),
            ("setting-off", "GSettings org.example.app disabled", false),
            ("setting-missing", "GSettings org.example.absent key", false),
            ("if-exists", "if-exists app/enabled", true),
            ("if-exists-missing", "if-exists app/missing", false),
            ("unless-exists", "unless-exists app/missing", true),
            ("unless-exists-present", "unless-exists app/enabled", false),
            ("if-session", "GNOME3 if-session tilo", true),
            ("if-other-session", "GNOME3 if-session gnome", false),
            ("unless-other-session", "GNOME3 unless-session gnome", true),
        ];
        for (id, condition, _) in cases {
            write(
                &dir,
                &format!("{id}.desktop"),
                &app(&format!("AutostartCondition={condition}\n")),
            );
        }
        write(
            &dir,
            "gconf.desktop",
            &app("AutostartCondition=GNOME /apps/example/enabled\n"),
        );
        let probe = Probe {
            settings: BTreeMap::from([
                (("org.example.app", "enabled"), true),
                (("org.example.app", "disabled"), false),
            ]),
            config_files: vec!["app/enabled"],
            ..Probe::default()
        };
        let scan = scan_autostart(&[dir], &probe, &[]);
        for (id, condition, starts) in cases {
            assert_eq!(
                started(&scan).contains(&id),
                starts,
                "{id}: AutostartCondition={condition}"
            );
            if !starts {
                assert_eq!(
                    reason(&scan, id),
                    Some(Skipped::ConditionFalse(condition.into()))
                );
            }
        }
        assert_eq!(
            reason(&scan, "gconf"),
            Some(Skipped::UnknownCondition(
                "GNOME /apps/example/enabled".into()
            ))
        );
    }

    #[test]
    fn missing_directories_are_simply_empty() {
        let scan = scan_autostart(
            &[PathBuf::from("/nonexistent/autostart")],
            &Probe::default(),
            &[],
        );
        assert!(scan.start.is_empty() && scan.skipped.is_empty());
    }
}
