//! One parsed `.desktop` file, per the freedesktop.org Desktop Entry Specification: `#`
//! comments, `[Group Name]` headers, `Key=Value`/`Key[locale]=Value` lines. Only
//! `[Desktop Entry]` is read here — `[Desktop Action foo]` groups become
//! [`DesktopEntry::actions`]. Values are unescaped per spec (`\s`/`\n`/`\\`), and lists are
//! `;`-separated with `\;` for a literal semicolon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `Type=` of an entry. Only [`EntryType::Application`] is launchable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Application,
    Link,
    Directory,
    /// A `Type=` we do not know. Kept rather than dropped so a caller can report "3 entries had
    /// an unrecognised Type" instead of the files silently vanishing.
    Other,
}

/// Where an entry came from, as far as it can be told from the file. What matters to the
/// desktop is *confinement*: a Flatpak carries its own sandbox and must not be wrapped in
/// Tilo's, while a plain system binary carries none — both are "host applications", not Tilo
/// packages, so this is a provenance label, not a permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntrySource {
    /// Exported by Flatpak. The payload is the `X-Flatpak=` application id, which is what
    /// `flatpak run` takes.
    Flatpak { app_id: String },
    /// Anything else: a distribution package, a locally-dropped file.
    Host,
}

impl EntrySource {
    /// Short label for UI. Deliberately never "Tilo" — nothing this module produces is a Tilo package.
    pub fn label(&self) -> &'static str {
        match self {
            EntrySource::Flatpak { .. } => "Flatpak",
            EntrySource::Host => "System",
        }
    }
}

/// What kind of app an entry is, as people tell apps apart: the one answer
/// the person's app list, search and `tilo app list` all give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Flatpak,
    AppImage,
    /// A program the person downloaded or put in their own applications
    /// directory.
    Program,
    /// One of the operating system's.
    System,
}

/// An extra launch verb declared by a `[Desktop Action <name>]` group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub exec: Option<String>,
}

/// How many main windows an app opens (`SingleMainWindow=`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MainWindows {
    /// Opened again, it opens another window: the spec's default.
    #[default]
    Many,
    /// `SingleMainWindow=true`: opened again, it raises the window it has.
    One,
}

/// A parsed `[Desktop Entry]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// The desktop-file ID: the path relative to the `applications` directory, `/` replaced by
    /// `-`, `.desktop` suffix removed (`kde/foo.desktop` → `kde-foo`). The identity the spec
    /// dedups on, and what most toolkits also use as the Wayland `app_id`.
    pub id: String,
    /// Absolute path of the file this came from.
    pub path: PathBuf,
    pub entry_type: EntryType,
    /// `Name=`, already locale-resolved. Never empty: an entry with no usable name falls back to its id.
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// Raw `Exec=` value, *unexpanded* — field codes are expanded at launch time by
    /// [`crate::exec`], since `%f`/`%u` depend on what the caller is opening.
    pub exec: Option<String>,
    /// `TryExec=` — a program whose presence gates the entry.
    pub try_exec: Option<String>,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub mime_types: Vec<String>,
    pub keywords: Vec<String>,
    /// `StartupWMClass=` — the window class the app sets. On Wayland this is what most toolkits
    /// report as the toplevel `app_id`, the primary handle for correlating a window with the
    /// entry that launched it.
    pub startup_wm_class: Option<String>,
    /// `NoDisplay=true`: valid and launchable, but not to be offered in a menu — usually a MIME
    /// handler or a helper.
    pub no_display: bool,
    /// `Hidden=true`: the spec's "deleted" tombstone. A user file with this set suppresses a
    /// system file of the same id.
    pub hidden: bool,
    pub terminal: bool,
    /// `SingleMainWindow=`: whether the app opens a second main window or,
    /// opened again, only raises the one it has.
    pub main_windows: MainWindows,
    /// `X-GNOME-Autostart-enabled`, `None` when absent: `Some(false)` switches an
    /// autostart entry off without deleting it.
    pub autostart_enabled: Option<bool>,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub source: EntrySource,
    pub actions: Vec<DesktopAction>,
    /// Set for the entry of a downloaded program ([`crate::downloaded`]):
    /// the file it starts, and whether that file is there.
    pub downloaded: Option<crate::downloaded::DownloadedProgram>,
}

impl DesktopEntry {
    /// What kind of app this is. `person_applications` is the person's own
    /// applications directory: an entry of theirs there is a program of
    /// theirs, not the system's.
    pub fn kind(&self, person_applications: Option<&Path>) -> EntryKind {
        if matches!(self.source, EntrySource::Flatpak { .. }) {
            return EntryKind::Flatpak;
        }
        if let Some(downloaded) = &self.downloaded {
            return match downloaded.kind {
                crate::downloaded::ProgramKind::AppImage => EntryKind::AppImage,
                crate::downloaded::ProgramKind::Program => EntryKind::Program,
            };
        }
        if person_applications.is_some_and(|dir| self.path.starts_with(dir)) {
            return EntryKind::Program;
        }
        EntryKind::System
    }

    /// Whether this entry should be offered in a menu of `desktop`. Four independent reasons to
    /// say no, all from the spec: wrong `Type`, `Hidden`, `NoDisplay`, and the
    /// `OnlyShowIn`/`NotShowIn` filter. `TryExec` is checked separately by the scanner, which can
    /// look at the filesystem.
    pub fn is_visible_in(&self, desktop: &[String]) -> bool {
        if self.entry_type != EntryType::Application || self.hidden || self.no_display {
            return false;
        }
        self.shows_in(desktop)
    }

    /// The `OnlyShowIn` / `NotShowIn` filter alone, without the `NoDisplay` / `Hidden` / `Type` checks.
    pub fn shows_in(&self, desktop: &[String]) -> bool {
        let matches = |list: &[String]| {
            list.iter()
                .any(|want| desktop.iter().any(|have| have.eq_ignore_ascii_case(want)))
        };
        if !self.only_show_in.is_empty() && !matches(&self.only_show_in) {
            return false;
        }
        if !self.not_show_in.is_empty() && matches(&self.not_show_in) {
            return false;
        }
        true
    }

    /// Every string a window's `app_id` might plausibly equal, most specific first. A foreign
    /// toplevel arrives with an `app_id` the desktop did not mint, so correlating it back to the
    /// entry means trying every handle the app could have used: `StartupWMClass` (what the spec
    /// says), the desktop-file id (GTK/Qt default), the Flatpak application id, and the basename
    /// of the program in `Exec`.
    pub fn window_identities(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |s: Option<String>| {
            if let Some(s) = s {
                let s = s.trim().to_string();
                if !s.is_empty() && !out.contains(&s) {
                    out.push(s);
                }
            }
        };
        push(self.startup_wm_class.clone());
        push(Some(self.id.clone()));
        if let EntrySource::Flatpak { app_id } = &self.source {
            push(Some(app_id.clone()));
        }
        push(self.exec_program_basename());
        out
    }

    /// Basename of `Exec`'s argv[0], if there is one. For a Flatpak that's `flatpak`, useless as
    /// an identity, so it's skipped rather than matching every Flatpak window to the first entry.
    fn exec_program_basename(&self) -> Option<String> {
        if matches!(self.source, EntrySource::Flatpak { .. }) {
            return None;
        }
        let exec = self.exec.as_deref()?;
        let argv = crate::exec::split_exec(exec).ok()?;
        let program = argv.first()?;
        Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .map(std::string::ToString::to_string)
    }
}

/// Parse the contents of a `.desktop` file. `id` and `path` are supplied by the scanner
/// (which knows the `applications` directory the file came from); `locales` is the caller's
/// preferred-locale list, most specific first (see [`crate::locale::locale_candidates`]).
///
/// Returns `None` only when the file has no `[Desktop Entry]` group at all — not a desktop
/// entry, rather than a broken one.
pub fn parse(contents: &str, id: &str, path: &Path, locales: &[String]) -> Option<DesktopEntry> {
    let groups = parse_groups(contents);
    let main = groups.get("Desktop Entry")?;

    let get = |key: &str| localised(main, key, locales);
    let raw = |key: &str| main.get(key).map(|v| v.default.as_str());

    let entry_type = match raw("Type").map(str::trim) {
        Some("Application") => EntryType::Application,
        Some("Link") => EntryType::Link,
        Some("Directory") => EntryType::Directory,
        _ => EntryType::Other,
    };

    let exec = raw("Exec").map(unescape).filter(|s| !s.trim().is_empty());
    let flatpak_id = raw("X-Flatpak")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        // Older exports (and `flatpak override`d ones) carry no X-Flatpak; the path is the fallback
        // signal, and the app id is then the desktop-file id, how Flatpak names exports.
        .or_else(|| is_flatpak_export_path(path).then(|| id.to_string()));
    let source = match flatpak_id {
        Some(app_id) => EntrySource::Flatpak { app_id },
        None => EntrySource::Host,
    };

    let name = get("Name")
        .map(|s| unescape(&s))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.to_string());

    Some(DesktopEntry {
        id: id.to_string(),
        path: path.to_path_buf(),
        entry_type,
        name,
        generic_name: get("GenericName")
            .map(|s| unescape(&s))
            .filter(|s| !s.is_empty()),
        comment: get("Comment")
            .map(|s| unescape(&s))
            .filter(|s| !s.is_empty()),
        exec,
        try_exec: raw("TryExec")
            .map(unescape)
            .filter(|s| !s.trim().is_empty()),
        icon: get("Icon")
            .map(|s| unescape(&s))
            .filter(|s| !s.trim().is_empty()),
        categories: string_list(raw("Categories")),
        mime_types: string_list(raw("MimeType")),
        keywords: string_list(get("Keywords").as_deref()),
        startup_wm_class: raw("StartupWMClass")
            .map(unescape)
            .filter(|s| !s.trim().is_empty()),
        no_display: boolean(raw("NoDisplay")),
        hidden: boolean(raw("Hidden")),
        terminal: boolean(raw("Terminal")),
        main_windows: if boolean(raw("SingleMainWindow")) {
            MainWindows::One
        } else {
            MainWindows::Many
        },
        autostart_enabled: raw("X-GNOME-Autostart-enabled").map(|value| boolean(Some(value))),
        only_show_in: string_list(raw("OnlyShowIn")),
        not_show_in: string_list(raw("NotShowIn")),
        source,
        actions: parse_actions(&groups, locales),
        downloaded: crate::downloaded::DownloadedProgram::from_keys(
            raw(crate::downloaded::KEY_PROGRAM),
            raw(crate::downloaded::KEY_KIND),
            raw(crate::downloaded::KEY_FILE),
            raw(crate::downloaded::KEY_LOCATIONS),
        ),
    })
}

/// `.../flatpak/exports/share/applications/x.desktop` — the two export roots Flatpak writes
/// into, system and per-user.
fn is_flatpak_export_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/flatpak/exports/")
}

fn parse_actions(groups: &BTreeMap<String, Group>, locales: &[String]) -> Vec<DesktopAction> {
    let mut out = Vec::new();
    for (header, group) in groups {
        let Some(id) = header.strip_prefix("Desktop Action ") else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let name =
            localised(group, "Name", locales).map_or_else(|| id.to_string(), |s| unescape(&s));
        out.push(DesktopAction {
            id: id.to_string(),
            name,
            exec: group.get("Exec").map(|v| unescape(&v.default)),
        });
    }
    out
}

// The key/value grammar

/// One key's values: the unlocalised one plus every `Key[locale]=`.
#[derive(Debug, Default, Clone)]
struct Value {
    default: String,
    localised: BTreeMap<String, String>,
}

type Group = BTreeMap<String, Value>;

fn parse_groups(contents: &str) -> BTreeMap<String, Group> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('[') {
            // A group header runs to the *last* `]` on the line so a group name containing one survives.
            if let Some(end) = rest.rfind(']') {
                // `end` is the byte offset `rfind` matched `]` at, always a char boundary.
                let header = rest.get(..end).unwrap_or(rest).to_string();
                groups.entry(header.clone()).or_default();
                current = Some(header);
            }
            continue;
        }
        let Some(group_name) = current.as_ref() else {
            // A key before any group header is not in the spec. Skip it rather than inventing a group.
            continue;
        };
        let Some((key_part, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key_part = key_part.trim();
        let value = value.trim().to_string();
        let group = groups.entry(group_name.clone()).or_default();
        if let Some((key, locale)) = key_part.split_once('[') {
            let Some(locale) = locale.strip_suffix(']') else {
                continue;
            };
            group
                .entry(key.trim().to_string())
                .or_default()
                .localised
                .insert(locale.trim().to_string(), value);
        } else {
            let slot = group.entry(key_part.to_string()).or_default();
            // First value wins, matching the spec's "duplicate keys are an error" by taking the one a
            // reader would have seen first.
            if slot.default.is_empty() {
                slot.default = value;
            }
        }
    }
    groups
}

/// Every group's unlocalised values, for the other key-file formats that share
/// this grammar (an icon theme's `index.theme`).
pub(crate) fn unlocalised_groups(contents: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    parse_groups(contents)
        .into_iter()
        .map(|(name, group)| {
            let values = group
                .into_iter()
                .map(|(key, value)| (key, value.default))
                .collect();
            (name, values)
        })
        .collect()
}

/// The best value for `key` given the caller's locale preference list. `locales` is already
/// ordered most-specific-first, so this is a straight first-hit lookup with the unlocalised
/// value as the floor.
fn localised(group: &Group, key: &str, locales: &[String]) -> Option<String> {
    let value = group.get(key)?;
    for want in locales {
        if let Some(hit) = value.localised.get(want) {
            return Some(hit.clone());
        }
    }
    if value.default.is_empty() {
        None
    } else {
        Some(value.default.clone())
    }
}

/// The spec's `true`/`false`. Anything else is false — including the `0`/`1` some old files
/// use, which the spec dropped.
fn boolean(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim), Some("true"))
}

/// A `;`-separated list value. `\;` is a literal semicolon; a customary trailing `;`
/// produces no empty final element.
pub(crate) fn string_list(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut item = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => item.push(';'),
                Some('\\') | None => item.push('\\'),
                Some('s') => item.push(' '),
                Some('n') => item.push('\n'),
                Some('t') => item.push('\t'),
                Some('r') => item.push('\r'),
                Some(other) => {
                    item.push('\\');
                    item.push(other);
                }
            },
            ';' => {
                let done = std::mem::take(&mut item);
                let done = done.trim().to_string();
                if !done.is_empty() {
                    out.push(done);
                }
            }
            other => item.push(other),
        }
    }
    let tail = item.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// The spec's string escapes, for non-list values.
pub fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPOTIFY: &str = "\
[Desktop Entry]
Type=Application
Name=Spotify
GenericName=Music Player
Icon=com.spotify.Client
Exec=/usr/bin/flatpak run --branch=stable --command=spotify --file-forwarding com.spotify.Client @@u %U @@
Terminal=false
MimeType=x-scheme-handler/spotify;
Categories=Audio;Music;Player;AudioVideo;
StartupWMClass=spotify
X-Flatpak=com.spotify.Client
";

    fn parse_str(s: &str, id: &str) -> DesktopEntry {
        parse(s, id, Path::new("/tmp/x.desktop"), &[]).expect("parses")
    }

    #[test]
    fn an_entry_is_a_flatpak_an_appimage_a_program_or_the_systems() {
        let mine = Path::new("/home/ana/.local/share/applications");
        let at = |path: &str, contents: &str| {
            parse(contents, "x", Path::new(path), &[]).expect("parses")
        };
        let plain = "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor\n";
        assert_eq!(
            at("/usr/share/applications/x.desktop", SPOTIFY).kind(Some(mine)),
            EntryKind::Flatpak
        );
        assert_eq!(
            at("/usr/share/applications/x.desktop", plain).kind(Some(mine)),
            EntryKind::System
        );
        assert_eq!(
            at("/home/ana/.local/share/applications/x.desktop", plain).kind(Some(mine)),
            EntryKind::Program
        );
        let appimage = format!(
            "{plain}X-Tilo-Downloaded-Program=/home/ana/Downloads/Editor.AppImage\nX-Tilo-Downloaded-Kind=AppImage\n"
        );
        assert_eq!(
            at(
                "/home/ana/.local/share/applications/tilo-downloaded/x.desktop",
                &appimage
            )
            .kind(Some(mine)),
            EntryKind::AppImage
        );
    }

    #[test]
    fn flatpak_and_host_entries_expose_their_launch_identities() {
        // Phase `parses_a_real_flatpak_export`.
        {
            let e = parse_str(SPOTIFY, "com.spotify.Client");
            assert_eq!(e.name, "Spotify");
            assert_eq!(e.entry_type, EntryType::Application);
            assert_eq!(
                e.source,
                EntrySource::Flatpak {
                    app_id: "com.spotify.Client".into()
                }
            );
            assert_eq!(e.startup_wm_class.as_deref(), Some("spotify"));
            assert_eq!(e.categories, vec!["Audio", "Music", "Player", "AudioVideo"]);
            assert_eq!(e.mime_types, vec!["x-scheme-handler/spotify"]);
            assert!(!e.terminal);
            assert!(!e.no_display);
        }
        // Phase `window_identities_skip_the_flatpak_binary`.
        // The identity list is what window correlation matches on. For a Flatpak it must never
        // include `flatpak` — that's the launcher, not the app, and every Flatpak would collide on it.
        {
            let e = parse_str(SPOTIFY, "com.spotify.Client");
            let ids = e.window_identities();
            assert_eq!(ids, vec!["spotify", "com.spotify.Client"]);
            assert!(!ids.iter().any(|i| i == "flatpak"));
        }
        // Phase `a_plain_host_entry_offers_its_exec_basename`.
        {
            let e = parse_str(
                "[Desktop Entry]\nType=Application\nName=Files\nExec=/usr/bin/nautilus %U\n",
                "org.gnome.Nautilus",
            );
            assert_eq!(e.source, EntrySource::Host);
            assert_eq!(
                e.window_identities(),
                vec!["org.gnome.Nautilus", "nautilus"]
            );
        }
        // Phase `a_flatpak_export_path_is_detected_without_the_x_flatpak_key`.
        {
            let e = parse(
                "[Desktop Entry]\nType=Application\nName=Old\nExec=x\n",
                "org.old.App",
                Path::new("/var/lib/flatpak/exports/share/applications/org.old.App.desktop"),
                &[],
            )
            .unwrap();
            assert_eq!(
                e.source,
                EntrySource::Flatpak {
                    app_id: "org.old.App".into()
                }
            );
        }
    }

    #[test]
    fn visibility_keys_filter_entries_on_the_current_desktop() {
        // Phase `hidden_and_nodisplay_are_distinct_and_both_hide`.
        {
            let hidden = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nHidden=true\n",
                "x",
            );
            let nodisplay = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n",
                "x",
            );
            assert!(hidden.hidden && !hidden.no_display);
            assert!(nodisplay.no_display && !nodisplay.hidden);
            assert!(!hidden.is_visible_in(&["tilo".into()]));
            assert!(!nodisplay.is_visible_in(&["tilo".into()]));
        }
        // Phase `only_show_in_and_not_show_in_filter_on_the_current_desktop`.
        {
            let gnome_only = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nOnlyShowIn=GNOME;\n",
                "x",
            );
            assert!(!gnome_only.is_visible_in(&["tilo".into()]));
            assert!(gnome_only.is_visible_in(&["GNOME".into()]));
            assert!(
                gnome_only.is_visible_in(&crate::scan::current_desktops()),
                "Tilo offers GNOME-only applications rather than hiding them"
            );
            let kde_only = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nOnlyShowIn=KDE;\n",
                "x",
            );
            assert!(!kde_only.is_visible_in(&crate::scan::current_desktops()));

            let not_tilo = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNotShowIn=tilo;KDE;\n",
                "x",
            );
            assert!(!not_tilo.is_visible_in(&["tilo".into()]));
            assert!(not_tilo.is_visible_in(&["GNOME".into()]));
        }
    }

    #[test]
    fn entry_fields_parse_locales_lists_actions_and_fallbacks() {
        // Phase `locale_lookup_prefers_the_most_specific_match`.
        {
            let src = "[Desktop Entry]\nType=Application\nName=Text Editor\n\
                       Name[de]=Texteditor\nName[de_AT]=Texteditor (AT)\nExec=x\n";
            let path = Path::new("/tmp/x.desktop");
            let de_at = parse(src, "x", path, &["de_AT".into(), "de".into()]).unwrap();
            assert_eq!(de_at.name, "Texteditor (AT)");
            let de = parse(src, "x", path, &["de".into()]).unwrap();
            assert_eq!(de.name, "Texteditor");
            let c = parse(src, "x", path, &[]).unwrap();
            assert_eq!(c.name, "Text Editor");
        }
        // Phase `escaped_semicolons_stay_inside_one_list_item`.
        {
            let e = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=A\\;B;C;\n",
                "x",
            );
            assert_eq!(e.categories, vec!["A;B", "C"]);
        }
        // Phase `a_file_with_no_desktop_entry_group_is_not_an_entry`.
        {
            assert!(parse("[Desktop Action foo]\nName=x\n", "x", Path::new("/x"), &[]).is_none());
        }
        // Phase `desktop_actions_are_collected`.
        {
            let e = parse_str(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nActions=new;\n\
                 [Desktop Action new]\nName=New Window\nExec=x --new\n",
                "x",
            );
            assert_eq!(e.actions.len(), 1);
            assert_eq!(e.actions[0].id, "new");
            assert_eq!(e.actions[0].name, "New Window");
            assert_eq!(e.actions[0].exec.as_deref(), Some("x --new"));
        }
        // Phase `a_missing_name_falls_back_to_the_id`.
        // An entry with no `Name=` still has to be launchable — the id is the fallback, not an
        // empty row in the palette.
        {
            let e = parse_str("[Desktop Entry]\nType=Application\nExec=x\n", "org.x.Thing");
            assert_eq!(e.name, "org.x.Thing");
        }
    }
}
