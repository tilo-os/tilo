//! The session's `mimeapps.list` chain, and the list Tilo publishes into it.

use std::path::{Path, PathBuf};

use desktop_policy::mimeapps::{CURRENT_DESKTOP, MimeAppsChain, MimeAppsList, search_path};

/// Where Tilo publishes its resolved host defaults: the first list on the chain for
/// `XDG_CURRENT_DESKTOP=tilo`, so `xdg-open`, `GLib` and Qt agree with the desktop.
pub fn published_list_path() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join(format!("{CURRENT_DESKTOP}-mimeapps.list")))
}

/// The session's chain as input to Tilo's own resolution. The list Tilo publishes is
/// derived from this resolution, so it is left out: reading it back would freeze
/// yesterday's fallback into today's choice.
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
}
