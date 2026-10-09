//! freedesktop.org MIME association lists (`mimeapps.list`).
//!
//! Host applications and the tools they call (`xdg-open`, `GLib`, Qt) decide
//! which program opens a file from an ordered chain of these lists. Tilo reads
//! the same chain so an association made with `xdg-mime` or a host app's own
//! "always open with" is honoured, and writes its resolved host defaults to
//! `$XDG_CONFIG_HOME/tilo-mimeapps.list` so those tools agree with the desktop.
//!
//! Host applications are named in policy by their desktop-file name,
//! `org.example.App.desktop`, which is exactly how these lists spell them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Suffix that marks a policy id as a host desktop entry rather than a Tilo package.
pub const HOST_APP_SUFFIX: &str = ".desktop";

/// The policy id of a host application with desktop-file id `desktop_id`.
pub fn host_app_id(desktop_id: &str) -> String {
    format!("{desktop_id}{HOST_APP_SUFFIX}")
}

/// The desktop-file id behind a host policy id, or `None` for a Tilo package.
pub fn host_desktop_id(policy_id: &str) -> Option<&str> {
    policy_id
        .strip_suffix(HOST_APP_SUFFIX)
        .filter(|id| !id.is_empty())
}

const DEFAULT_APPLICATIONS: &str = "Default Applications";
const ADDED_ASSOCIATIONS: &str = "Added Associations";
const REMOVED_ASSOCIATIONS: &str = "Removed Associations";

/// One parsed `mimeapps.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeAppsList {
    pub defaults: BTreeMap<String, Vec<String>>,
    pub added: BTreeMap<String, Vec<String>>,
    pub removed: BTreeMap<String, Vec<String>>,
}

impl MimeAppsList {
    /// Forgiving keyfile parse: unknown groups, comments and malformed lines are skipped.
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        let mut group: Option<&str> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                group = Some(name.trim());
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let target = match group {
                Some(DEFAULT_APPLICATIONS) => &mut list.defaults,
                Some(ADDED_ASSOCIATIONS) => &mut list.added,
                Some(REMOVED_ASSOCIATIONS) => &mut list.removed,
                _ => continue,
            };
            let mime = key.trim().to_ascii_lowercase();
            if mime.is_empty() {
                continue;
            }
            let ids = value
                .split(';')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            target.entry(mime).or_default().extend(ids);
        }
        list
    }

    /// Serialise in the keyfile form every consumer of the spec reads.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (name, group) in [
            (DEFAULT_APPLICATIONS, &self.defaults),
            (ADDED_ASSOCIATIONS, &self.added),
            (REMOVED_ASSOCIATIONS, &self.removed),
        ] {
            if group.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push('[');
            out.push_str(name);
            out.push_str("]\n");
            for (mime, ids) in group {
                out.push_str(mime);
                out.push('=');
                for id in ids {
                    out.push_str(id);
                    out.push(';');
                }
                out.push('\n');
            }
        }
        out
    }

    /// Read one list; a missing or unreadable file is an empty list.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }
}

/// The lookup order the specification defines, most important first. `desktops` are the
/// `XDG_CURRENT_DESKTOP` entries; each contributes a `<desktop>-mimeapps.list` ahead of the
/// plain one in every directory.
pub fn search_path(
    desktops: &[String],
    config_home: Option<&Path>,
    config_dirs: &[PathBuf],
    data_home: Option<&Path>,
    data_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let names: Vec<String> = desktops
        .iter()
        .filter(|desktop| !desktop.is_empty())
        .map(|desktop| format!("{}-mimeapps.list", desktop.to_ascii_lowercase()))
        .chain(std::iter::once("mimeapps.list".to_string()))
        .collect();
    let applications = |dir: &Path| dir.join("applications");
    let directories: Vec<PathBuf> = config_home
        .map(Path::to_path_buf)
        .into_iter()
        .chain(config_dirs.iter().cloned())
        .chain(data_home.map(applications))
        .chain(data_dirs.iter().map(|dir| applications(dir)))
        .collect();
    let mut out = Vec::new();
    for directory in directories {
        for name in &names {
            let path = directory.join(name);
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// The ordered chain, most important list first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeAppsChain {
    lists: Vec<MimeAppsList>,
}

impl MimeAppsChain {
    pub fn new(lists: Vec<MimeAppsList>) -> Self {
        Self { lists }
    }

    /// Load every list on `paths`, in order.
    pub fn load(paths: &[PathBuf]) -> Self {
        Self::new(paths.iter().map(|path| MimeAppsList::load(path)).collect())
    }

    /// Whether host app `desktop_file` (`foo.desktop`) handles `mime`, given whether its own
    /// `MimeType=` declares it. An association added by a list survives only removals made in
    /// that list or a more important one; a declared type is removed by any list.
    pub fn handles(&self, desktop_file: &str, mime: &str, declared: bool) -> bool {
        let mut removed = false;
        for list in &self.lists {
            let lists_id = |group: &BTreeMap<String, Vec<String>>| {
                group
                    .get(mime)
                    .is_some_and(|ids| ids.iter().any(|id| id == desktop_file))
            };
            removed |= lists_id(&list.removed);
            if lists_id(&list.added) && !removed {
                return true;
            }
        }
        declared && !removed
    }

    /// Every MIME type some list adds for `desktop_file`, before removals are applied.
    pub fn added_types(&self, desktop_file: &str) -> BTreeSet<String> {
        self.lists
            .iter()
            .flat_map(|list| list.added.iter())
            .filter(|(_, ids)| ids.iter().any(|id| id == desktop_file))
            .map(|(mime, _)| mime.clone())
            .collect()
    }

    /// The first `Default Applications` entry for `mime`, in chain order, that `usable`
    /// accepts. Entries naming an application that is not installed, or no longer handles the
    /// type, are skipped as the specification requires.
    pub fn default_for(&self, mime: &str, usable: impl Fn(&str) -> bool) -> Option<&str> {
        self.lists
            .iter()
            .filter_map(|list| list.defaults.get(mime))
            .flatten()
            .map(String::as_str)
            .find(|id| usable(id))
    }
}

/// The desktop name Tilo announces in `XDG_CURRENT_DESKTOP`.
pub const CURRENT_DESKTOP: &str = "tilo";

/// Where Tilo publishes its resolved host defaults: the first list on the chain for
/// `XDG_CURRENT_DESKTOP=tilo`, so `xdg-open`, `GLib` and Qt agree with the desktop.
#[cfg(feature = "host-storage")]
pub fn published_list_path() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join(format!("{CURRENT_DESKTOP}-mimeapps.list")))
}

/// The session's chain as input to Tilo's own resolution. The list Tilo publishes is
/// derived from this resolution, so it is left out: reading it back would freeze
/// yesterday's fallback into today's choice.
#[cfg(feature = "host-storage")]
pub fn session_chain() -> MimeAppsChain {
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|value| !value.is_empty())
        .map_or_else(
            || vec![CURRENT_DESKTOP.to_string()],
            |value| value.split(':').map(str::to_string).collect(),
        );
    let published = published_list_path();
    let paths: Vec<PathBuf> = search_path(
        &desktops,
        tilo_paths::xdg::config_home().as_deref(),
        &tilo_paths::xdg::config_dirs(),
        tilo_paths::xdg::data_home().as_deref(),
        &tilo_paths::xdg::data_dirs(),
    )
    .into_iter()
    .filter(|path| Some(path) != published.as_ref())
    .collect();
    MimeAppsChain::load(&paths)
}

/// Replace `path` with `list`, crash-safely, skipping the write when nothing changed.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
#[cfg(feature = "host-storage")]
pub fn publish(path: &Path, list: &MimeAppsList) -> std::io::Result<bool> {
    let mut body = String::from(
        "# Written by the Tilo desktop from its resolved default applications.\n\
         # Change defaults in Settings; edits here are replaced.\n",
    );
    body.push_str(&list.render());
    if std::fs::read_to_string(path).is_ok_and(|current| current == body) {
        return Ok(false);
    }
    tilo_files::native::atomic::write_creating_parent(path, body.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "host-storage")]
    #[test]
    fn publish_writes_a_list_host_tools_read_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo-mimeapps.list");
        let mut list = MimeAppsList::default();
        list.defaults.insert(
            "application/pdf".into(),
            vec!["org.gnome.Evince.desktop".into()],
        );
        assert!(publish(&path, &list).unwrap());
        assert!(
            !publish(&path, &list).unwrap(),
            "unchanged content is not rewritten"
        );
        let written = MimeAppsList::load(&path);
        assert_eq!(written.defaults, list.defaults);
    }

    #[test]
    fn parse_reads_the_three_groups_and_ignores_the_rest() {
        let list = MimeAppsList::parse(
            "# comment\n[Default Applications]\nText/Plain=gedit.desktop;vim.desktop;\n\
             [Added Associations]\nimage/png=gimp.desktop\n[Removed Associations]\n\
             image/png=eog.desktop;\n[Other]\nx/y=z.desktop\nnot a line\n",
        );
        assert_eq!(
            list.defaults["text/plain"],
            vec!["gedit.desktop", "vim.desktop"]
        );
        assert_eq!(list.added["image/png"], vec!["gimp.desktop"]);
        assert_eq!(list.removed["image/png"], vec!["eog.desktop"]);
        assert!(!list.defaults.contains_key("x/y"));
        assert_eq!(MimeAppsList::parse(&list.render()), list);
    }

    #[test]
    fn search_path_follows_the_specification_order() {
        let paths = search_path(
            &["tilo".into()],
            Some(Path::new("/home/u/.config")),
            &[PathBuf::from("/etc/xdg")],
            Some(Path::new("/home/u/.local/share")),
            &[PathBuf::from("/usr/share")],
        );
        let expected: Vec<PathBuf> = [
            "/home/u/.config/tilo-mimeapps.list",
            "/home/u/.config/mimeapps.list",
            "/etc/xdg/tilo-mimeapps.list",
            "/etc/xdg/mimeapps.list",
            "/home/u/.local/share/applications/tilo-mimeapps.list",
            "/home/u/.local/share/applications/mimeapps.list",
            "/usr/share/applications/tilo-mimeapps.list",
            "/usr/share/applications/mimeapps.list",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(paths, expected);
    }

    #[test]
    fn removals_bind_lower_lists_but_not_more_important_additions() {
        let user = MimeAppsList::parse("[Added Associations]\nimage/png=gimp.desktop;\n");
        let system = MimeAppsList::parse(
            "[Removed Associations]\nimage/png=gimp.desktop;eog.desktop;\n\
             [Added Associations]\nimage/png=krita.desktop;\n",
        );
        let override_system =
            MimeAppsList::parse("[Removed Associations]\nimage/png=krita.desktop;\n");
        let chain = MimeAppsChain::new(vec![user, override_system, system]);
        assert!(
            chain.handles("gimp.desktop", "image/png", false),
            "the user's addition outranks the system removal"
        );
        assert!(
            !chain.handles("eog.desktop", "image/png", true),
            "a removal hides a declared type"
        );
        assert!(
            !chain.handles("krita.desktop", "image/png", false),
            "a more important removal hides a lower addition"
        );
        assert!(chain.handles("other.desktop", "image/png", true));
        assert_eq!(
            chain.added_types("krita.desktop"),
            BTreeSet::from(["image/png".to_string()])
        );
    }

    #[test]
    fn default_skips_entries_that_are_not_usable() {
        let chain = MimeAppsChain::new(vec![
            MimeAppsList::parse("[Default Applications]\ntext/plain=gone.desktop;\n"),
            MimeAppsList::parse("[Default Applications]\ntext/plain=gedit.desktop;\n"),
        ]);
        assert_eq!(
            chain.default_for("text/plain", |id| id == "gedit.desktop"),
            Some("gedit.desktop")
        );
        assert_eq!(chain.default_for("text/plain", |_| false), None);
    }

    #[test]
    fn host_ids_round_trip_and_packages_are_not_host_apps() {
        assert_eq!(host_app_id("org.gnome.Evince"), "org.gnome.Evince.desktop");
        assert_eq!(
            host_desktop_id("org.gnome.Evince.desktop"),
            Some("org.gnome.Evince")
        );
        assert_eq!(host_desktop_id("so.tilo.pdf-viewer"), None);
        assert_eq!(host_desktop_id(".desktop"), None);
        assert!(crate::defaults::validate_package_id(&host_app_id("kde-okular")).is_ok());
    }
}
