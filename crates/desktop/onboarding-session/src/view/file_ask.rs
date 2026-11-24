//! The file setup asks the person for, as the system's question: the files
//! `FilesAsked` names, offered in the shared file picker over the page.
//!
//! Restoring asks for the backup to open, so the person can see the files they
//! keep; saving a backup asks where it goes. The question carries no title of
//! its own: the picker shows what the step is for, and dismissing it leaves the
//! step where it was, nothing chosen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use taffy::TaffyTree;
use tilo_actions::remote::asked::{FileType, FilesAsked, FilesMode};
use ui::widget::file_picker::FileFilter;
use ui::widget::{FilePicker, FilePickerMode, FilePickerOutcome, Widget};
use vello::kurbo::Rect;
use winit::event::WindowEvent;

use tilo_onboarding::t;

/// What the person is choosing a file for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AskPurpose {
    /// The backup to restore the identity from.
    Restore,
    /// Where a new backup is saved.
    Save,
}

/// What the person answered: a file, or dismissal.
pub(super) enum Answered {
    Chosen(PathBuf),
    Dismissed,
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

/// The question standing over the page, with the picker that puts it to the person.
pub(super) struct FileAsk {
    purpose: AskPurpose,
    question: FilesAsked,
    picker: FilePicker,
}

impl FileAsk {
    /// The question `question` put in the picker, opened at `initial`, which
    /// may look under `roots`.
    #[must_use]
    pub(super) fn new(
        purpose: AskPurpose,
        question: FilesAsked,
        initial: &Path,
        roots: Vec<PathBuf>,
    ) -> Self {
        let title = match purpose {
            AskPurpose::Restore => t("onboarding-picker-import-title"),
            AskPurpose::Save => t("onboarding-picker-backup-title"),
        };
        let mode = match question.mode {
            FilesMode::Open => FilePickerMode::Open,
            FilesMode::Save => FilePickerMode::Save,
            FilesMode::Folder => FilePickerMode::Directory,
        };
        let mut filters: Vec<FileFilter> = question
            .types
            .iter()
            .map(|ty| {
                let patterns: Vec<&str> = ty.patterns.iter().map(String::as_str).collect();
                FileFilter::new(ty.label.clone(), &patterns)
            })
            .collect();
        if question.mode == FilesMode::Open {
            // A backup may be named anything the person kept it under.
            filters.push(FileFilter::all());
        }
        let mut picker = FilePicker::new(title, initial, roots)
            .with_mode(mode)
            .with_filters(filters)
            .with_waker(Some(waker()));
        if let Some(name) = &question.name {
            picker = picker.with_filename(name.clone());
        }
        Self {
            purpose,
            question,
            picker,
        }
    }

    #[must_use]
    pub(super) fn purpose(&self) -> AskPurpose {
        self.purpose
    }

    /// The system's question as the person is asked it.
    #[must_use]
    pub(super) fn question(&self) -> &FilesAsked {
        &self.question
    }

    pub(super) fn set_rect(&mut self, rect: Rect) {
        self.picker.set_rect(rect);
    }

    #[must_use]
    pub(super) fn picker(&self) -> &FilePicker {
        &self.picker
    }

    pub(super) fn picker_mut(&mut self) -> &mut FilePicker {
        &mut self.picker
    }

    /// Look again at the folder the person moved to. Whether what it shows
    /// changed.
    pub(super) fn poll_lookups(&mut self) -> bool {
        self.picker.poll_lookups()
    }

    /// What the person answered, once they have.
    pub(super) fn take(&mut self) -> Option<Answered> {
        match self.picker.take_outcome()? {
            FilePickerOutcome::Selected(mut paths) => paths.pop().map(Answered::Chosen),
            FilePickerOutcome::Cancelled => Some(Answered::Dismissed),
        }
    }

    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let mut picker_taffy = TaffyTree::<()>::new();
        if let Ok(node) = picker_taffy.new_leaf(taffy::Style::default()) {
            let path = ctx.path;
            self.picker.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    taffy: &picker_taffy,
                    ..ctx.reborrow(path, node)
                },
            );
        }
    }

    pub(super) fn draw(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        use ui::widget::DrawChild as _;
        let (scene, transform, text_renderer, theme, env) = ctx.parts();
        self.picker
            .draw_child(scene, transform, text_renderer, theme, &env);
    }

    #[must_use]
    pub(super) fn needs_animation_frame(&self) -> bool {
        self.picker.needs_animation_frame()
    }

    pub(super) fn take_structure_dirty(&mut self) -> bool {
        self.picker.take_structure_dirty()
    }
}

/// Where the question stands over `viewport`: centred, most of what a person
/// sees, never wider or taller than it.
#[must_use]
pub(super) fn rect(viewport: Rect) -> Rect {
    let width = (viewport.width() * 0.78)
        .clamp(420.0, 900.0)
        .min(viewport.width());
    let height = (viewport.height() * 0.78)
        .clamp(320.0, 640.0)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - width) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - height) / 2.0;
    Rect::new(x0, y0, x0 + width, y0 + height)
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
