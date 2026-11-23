//! freedesktop.org MIME association lists (`mimeapps.list`).
//!
//! Host applications and the tools they call (`xdg-open`, `GLib`, Qt) decide which
//! program opens a file from an ordered chain of these lists. Tilo reads the same
//! chain, so an association made with `xdg-mime` or a host app's "always open with"
//! is honoured, and writes its resolved host defaults to
//! `$XDG_CONFIG_HOME/tilo-mimeapps.list` so those tools agree with the desktop.
//!
//! Host applications are named by their desktop-file name, `org.example.App.desktop`,
//! which is how these lists spell them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const HOST_APP_SUFFIX: &str = ".desktop";

pub fn host_app_id(desktop_id: &str) -> String {
    format!("{desktop_id}{HOST_APP_SUFFIX}")
}

pub fn host_desktop_id(policy_id: &str) -> Option<&str> {
    policy_id
        .strip_suffix(HOST_APP_SUFFIX)
        .filter(|id| !id.is_empty())
}

const DEFAULT_APPLICATIONS: &str = "Default Applications";
const ADDED_ASSOCIATIONS: &str = "Added Associations";
const REMOVED_ASSOCIATIONS: &str = "Removed Associations";

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

/// The lookup order the specification defines, most important first. `desktops`
/// are the `XDG_CURRENT_DESKTOP` entries; each contributes a
/// `<desktop>-mimeapps.list` ahead of the plain one in every directory.
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeAppsChain {
    lists: Vec<MimeAppsList>,
}

impl MimeAppsChain {
    pub fn new(lists: Vec<MimeAppsList>) -> Self {
        Self { lists }
    }

    pub fn load(paths: &[PathBuf]) -> Self {
        Self::new(paths.iter().map(|path| MimeAppsList::load(path)).collect())
    }

    /// Whether host app `desktop_file` (`foo.desktop`) handles `mime`, given
    /// whether its own `MimeType=` declares it. An association a list adds survives
    /// unless that list or a more important one removes it; a declared type is
    /// removed by any list.
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

    pub fn added_types(&self, desktop_file: &str) -> BTreeSet<String> {
        self.lists
            .iter()
            .flat_map(|list| list.added.iter())
            .filter(|(_, ids)| ids.iter().any(|id| id == desktop_file))
            .map(|(mime, _)| mime.clone())
            .collect()
    }

    /// The first `Default Applications` entry for `mime`, in chain order, that
    /// `usable` accepts. Entries naming an application that is not installed, or no
    /// longer handles the type, are skipped as the specification requires.
    pub fn default_for(&self, mime: &str, usable: impl Fn(&str) -> bool) -> Option<&str> {
        self.lists
            .iter()
            .filter_map(|list| list.defaults.get(mime))
            .flatten()
            .map(String::as_str)
            .find(|id| usable(id))
    }
}

pub const CURRENT_DESKTOP: &str = "tilo";

/// Where Tilo publishes its resolved host defaults: the first list on the chain
/// for `XDG_CURRENT_DESKTOP=tilo`, so `xdg-open`, `GLib` and Qt agree with the
/// desktop.
#[cfg(feature = "host-storage")]
pub fn published_list_path() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join(format!("{CURRENT_DESKTOP}-mimeapps.list")))
}

/// The session's chain, as input to Tilo's own resolution. The list Tilo publishes
/// comes from this resolution, so it is left out: reading it back would freeze
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

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A person runs `xdg-mime default` and a host app's "always open with" writes
    /// the same lists; Tilo reads that whole chain, publishes its choice into the
    /// list the host's own tools read, and both agree.
    #[test]
    fn the_hosts_lists_decide_as_the_specification_says() {
        let home = tempfile::tempdir().unwrap();
        let config_home = home.path().join("config");
        let etc = home.path().join("etc");
        let data_home = home.path().join("share");
        let usr = home.path().join("usr");

        // xdg-mime writes the person's own list; a host app's association lands beside
        // it. A package manager's system list is the last word only for what nobody
        // above it says.
        write(
            &config_home.join("mimeapps.list"),
            "# written by xdg-mime\n[Added Associations]\nimage/png=gimp.desktop;\n\
             [Default Applications]\nText/Plain=gone.desktop;gedit.desktop;\n\
             not a line\n[Other]\nx/y=z.desktop\n",
        );
        write(
            &data_home.join("applications/mimeapps.list"),
            "[Removed Associations]\nimage/png=gimp.desktop;eog.desktop;\n\
             [Added Associations]\nimage/png=krita.desktop;\n",
        );
        write(
            &config_home.join("tilo-mimeapps.list"),
            "[Removed Associations]\nimage/png=krita.desktop;\n",
        );

        let paths = search_path(
            &["tilo".into()],
            Some(&config_home),
            std::slice::from_ref(&etc),
            Some(&data_home),
            std::slice::from_ref(&usr),
        );
        let names = ["tilo-mimeapps.list", "mimeapps.list"];
        let expected: Vec<PathBuf> = [&config_home, &etc]
            .into_iter()
            .flat_map(|dir| names.map(|name| dir.join(name)))
            .chain(
                [data_home.join("applications"), usr.join("applications")]
                    .into_iter()
                    .flat_map(|dir| names.map(move |name| dir.clone().join(name))),
            )
            .collect();
        assert_eq!(paths, expected, "the specification's lookup order");

        let chain = MimeAppsChain::load(&paths);
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
        assert_eq!(
            chain.default_for("text/plain", |id| id == "gedit.desktop"),
            Some("gedit.desktop"),
            "an entry naming a program this device no longer has is skipped"
        );
        assert_eq!(chain.default_for("text/plain", |_| false), None);

        let list = MimeAppsList::load(&config_home.join("mimeapps.list"));
        assert_eq!(
            list.defaults["text/plain"],
            vec!["gone.desktop", "gedit.desktop"]
        );
        assert_eq!(MimeAppsList::parse(&list.render()), list);
        assert!(!list.defaults.contains_key("x/y"));

        #[cfg(feature = "host-storage")]
        {
            let published = config_home.join("tilo-mimeapps.list");
            let mut list = MimeAppsList::default();
            list.defaults.insert(
                "application/pdf".into(),
                vec!["org.gnome.Evince.desktop".into()],
            );
            assert!(publish(&published, &list).unwrap());
            assert!(
                !publish(&published, &list).unwrap(),
                "unchanged content is not rewritten"
            );
            let written = std::fs::read_to_string(&published).unwrap();
            assert!(written.starts_with("# Written by the Tilo desktop"));
            assert_eq!(MimeAppsList::load(&published).defaults, list.defaults);
        }
    }

    #[test]
    fn host_ids_name_desktop_files_and_never_a_package() {
        assert_eq!(host_app_id("org.gnome.Evince"), "org.gnome.Evince.desktop");
        assert_eq!(
            host_desktop_id("org.gnome.Evince.desktop"),
            Some("org.gnome.Evince")
        );
        assert_eq!(host_desktop_id("so.tilo.pdf-viewer"), None);
        assert_eq!(host_desktop_id(".desktop"), None);
    }
}
