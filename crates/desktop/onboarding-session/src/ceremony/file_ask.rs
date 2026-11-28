//! The file setup asks the person for, as the system's question: the files
//! `FilesAsked` names, offered in the shared file picker over the page.
//!
//! Restoring asks for the backup to open, so the person can see the files they
//! keep; saving a backup asks where it goes. The question carries no title of
//! its own: the picker shows what the step is for, and dismissing it leaves the
//! step where it was, nothing chosen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_actions::remote::asked::{FileType, FilesAsked, FilesMode};
use tilo_ui_file_picker::FileFilter;
use tilo_ui_file_picker::{FilePicker, FilePickerMode};

use crate::t;

/// What the person is choosing a file for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AskPurpose {
    /// The backup to restore the identity from.
    Restore,
    /// Where a new backup is saved.
    Save,
}

/// The question setup asks about `purpose`, opened at `initial`.
#[must_use]
pub(super) fn question(purpose: AskPurpose, initial: &Path) -> FilesAsked {
    let folder = initial.to_str().map(str::to_owned);
    match purpose {
        AskPurpose::Restore => FilesAsked {
            mode: FilesMode::Open,
            many: false,
            types: vec![FileType {
                label: t("onboarding-picker-import-filter"),
                patterns: ["tilo-identity", "tilo-identity-backup", "json"]
                    .map(str::to_owned)
                    .to_vec(),
            }],
            name: None,
            folder,
        },
        AskPurpose::Save => FilesAsked {
            mode: FilesMode::Save,
            many: false,
            types: vec![FileType {
                label: t("onboarding-picker-backup-filter"),
                patterns: vec!["tilo-identity".to_owned()],
            }],
            name: Some("tilo-identity-backup.tilo-identity".to_owned()),
            folder,
        },
    }
}

/// The question standing over the page, which the session puts to the person in the shared file
/// picker ([`FileAsk::picker`]) and answers with [`super::Ceremony::files_answered`].
pub struct FileAsk {
    purpose: AskPurpose,
    question: FilesAsked,
    initial: PathBuf,
    roots: Vec<PathBuf>,
}

impl FileAsk {
    /// The question `question` about `purpose`, opened at `initial`, which may look under `roots`.
    #[must_use]
    pub(super) fn new(
        purpose: AskPurpose,
        question: FilesAsked,
        initial: &Path,
        roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            purpose,
            question,
            initial: initial.to_owned(),
            roots,
        }
    }

    #[must_use]
    pub(super) fn purpose(&self) -> AskPurpose {
        self.purpose
    }

    /// The system's question as the person is asked it.
    #[must_use]
    pub fn question(&self) -> &FilesAsked {
        &self.question
    }

    /// The shared picker that puts the question to the person: titled by what the step is for,
    /// filtered to the files it takes, and in its own folder.
    #[must_use]
    pub fn picker(&self) -> FilePicker {
        let title = match self.purpose {
            AskPurpose::Restore => t("onboarding-picker-import-title"),
            AskPurpose::Save => t("onboarding-picker-backup-title"),
        };
        let mode = match self.question.mode {
            FilesMode::Open => FilePickerMode::Open,
            FilesMode::Save => FilePickerMode::Save,
            FilesMode::Folder => FilePickerMode::Directory,
        };
        let mut filters: Vec<FileFilter> = self
            .question
            .types
            .iter()
            .map(|ty| {
                let patterns: Vec<&str> = ty.patterns.iter().map(String::as_str).collect();
                FileFilter::new(ty.label.clone(), &patterns)
            })
            .collect();
        if self.question.mode == FilesMode::Open {
            // A backup may be named anything the person kept it under.
            filters.push(FileFilter::all());
        }
        let mut picker = FilePicker::new(title, &self.initial, self.roots.clone())
            .with_mode(mode)
            .with_filters(filters)
            .with_waker(Some(waker()));
        if let Some(name) = &self.question.name {
            picker = picker.with_filename(name.clone());
        }
        picker
    }
}

/// The session loop's waker, so a folder read reports when it lands.
fn waker() -> std::task::Waker {
    std::task::Waker::from(Arc::new(Wake(crate::services::loop_waker())))
}

struct Wake(std::task::Waker);

impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) {
        self.0.wake_by_ref();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restoring asks for the backup to open, at the person's own files, and
    /// offers every file as well as the identity ones.
    #[test]
    fn restoring_asks_for_a_backup_to_open() {
        let asked = question(AskPurpose::Restore, Path::new("/home/person"));
        assert_eq!(asked.mode, FilesMode::Open);
        assert_eq!(asked.folder.as_deref(), Some("/home/person"));
        assert!(asked.name.is_none(), "nothing is being saved");
        assert!(
            asked
                .types
                .iter()
                .any(|ty| ty.patterns.iter().any(|pattern| pattern == "tilo-identity")),
            "the identity's own files come first: {asked:?}"
        );
    }

    /// Saving a backup asks where it goes, named as the backup it writes.
    #[test]
    fn saving_a_backup_asks_where_it_goes() {
        let asked = question(AskPurpose::Save, Path::new("/home/person"));
        assert_eq!(asked.mode, FilesMode::Save);
        assert_eq!(
            asked.name.as_deref(),
            Some("tilo-identity-backup.tilo-identity")
        );
        assert_eq!(asked.folder.as_deref(), Some("/home/person"));
    }
}
