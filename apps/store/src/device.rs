//! What this device's Flatpak installation has, read from what Flatpak keeps on the device:
//! the `AppStream` metadata the Store and system search already share, and the desktop entries
//! Flatpak exports for the apps it has installed. Nothing here starts a package manager or asks
//! a service: a Flatpak app installs through the workspace's `workspace/apps` actions, which the
//! device's daemon carries out.

use crate::flathub_client::Remote;

/// Flatpak availability and the raw rows the Store's model reads: one installed app per line,
/// and one remote per line.
#[derive(Clone, Debug, Default)]
pub struct FlatpakStatus {
    pub available: bool,
    /// `application\tversion\torigin\tinstallation` per installed app.
    pub installed: String,
    /// `name\turl\ttitle\toptions` per remote.
    pub remotes: String,
}

/// Whether this device has Flatpak, and the apps it has installed.
#[must_use]
pub fn status() -> FlatpakStatus {
    // Flatpak keeps its installation's `AppStream` under a directory of its own: one there means
    // Flatpak is installed, whether or not this process may run it.
    let available = app_catalogue::appstream::cache_directories()
        .iter()
        .any(|dir| dir.is_dir());
    if !available {
        return FlatpakStatus::default();
    }
    let locales =
        desktop_entry::locale::language_candidates(ui::i18n::preferred_language().as_deref());
    let scan = desktop_entry::cache::cached_scan(&locales);
    let installed = scan
        .entries
        .iter()
        .filter_map(|entry| match &entry.source {
            desktop_entry::EntrySource::Flatpak { app_id } => {
                Some(format!("{app_id}\t\tflathub\tsystem"))
            }
            desktop_entry::EntrySource::Host => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    FlatpakStatus {
        available: true,
        installed,
        // A remote is the device's: what it reads from is not a person's to list here.
        remotes: String::new(),
    }
}

/// The applications `remote` offers, as `application\tname\tdescription\tversion\tbranch\torigin`
/// rows: every app the Flatpak metadata on this device names. The metadata does not say which
/// remote an app came from, so a row names the remote it was read through.
#[must_use]
pub fn catalogue(remote: &Remote) -> String {
    let entries = app_catalogue::appstream::load_cached(&crate::catalogue::desktop_locale());
    entries
        .into_iter()
        .filter(|entry| matches!(&entry.source, app_catalogue::Source::Flathub { .. }))
        .map(|entry| {
            let description = if entry.tagline.is_empty() {
                entry.description
            } else {
                entry.tagline
            };
            format!(
                "{}\t{}\t{}\t\tstable\t{}",
                entry.id,
                entry.name.replace(['\t', '\n'], " "),
                description.replace(['\t', '\n'], " "),
                remote.name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
