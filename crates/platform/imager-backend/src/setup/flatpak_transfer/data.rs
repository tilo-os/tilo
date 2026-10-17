use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataSubtree {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub journal: PathBuf,
}

/// Entire user installation includes signed objects, remotes, runtimes, app
/// deployments, overrides and portal permission-store state. `.var/app` also
/// preserves data for apps no longer installed. Whole-home migration already
/// transfers both: returning no jobs prevents duplicate writes or merges.
pub fn data_subtrees(
    source_home: &Path,
    target_home: &Path,
    private_journals: &Path,
    data_already_transferred: bool,
) -> Result<Vec<DataSubtree>, String> {
    if !source_home.is_absolute() || !target_home.is_absolute() || !private_journals.is_absolute() {
        return Err("Flatpak transfer requires absolute directory grants".into());
    }
    if data_already_transferred {
        return Ok(Vec::new());
    }
    Ok([
        (".local/share/flatpak", "flatpak-installation"),
        (".var/app", "flatpak-app-data"),
    ]
    .into_iter()
    .map(|(relative, journal)| DataSubtree {
        source: source_home.join(relative),
        destination: target_home.join(relative),
        journal: private_journals.join(journal),
    })
    .collect())
}

/// The target parent directories and journal directory must already have been
/// privately created by target-user setup. Existing destination subtrees refuse
/// replacement. The same byte verification, fidelity and durable resume path as
/// whole-home migration performs this transfer, including deployment symlinks.
#[cfg(target_os = "linux")]
pub fn review_subtree(
    subtree: &DataSubtree,
) -> Result<super::super::home_transfer::HomeReview, String> {
    super::super::home_transfer::review(&subtree.source, &subtree.destination, &subtree.journal)
}
