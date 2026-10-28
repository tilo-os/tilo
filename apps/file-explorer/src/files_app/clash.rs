//! Name clashes when copying or moving: one question for all of them.
//!
//! Copying or moving into a folder that already has items of the same names
//! asks once: Replace (the existing items go to Trash, so nothing is lost),
//! Keep both (the new ones get free names), or Skip (only the others go).
//! The question is a sheet over the window, answered by pointer, keyboard
//! or assistive technology.

use std::path::{Path, PathBuf};

use crate::i18n::{arg, t, t_args};
use ui::widget::modal_dialog::{self, ModalAction};
use ui::widget::{ModalHit, ModalLayout, ModalSpec};
use vello::kurbo::{Point, Rect};

/// How to settle the clashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClashChoice {
    Skip,
    KeepBoth,
    Replace,
}

impl ClashChoice {
    /// In the order the sheet shows them; the last is the primary.
    const ALL: [Self; 3] = [Self::Skip, Self::KeepBoth, Self::Replace];

    fn label(self) -> String {
        t(match self {
            Self::Skip => "files-clash-skip",
            Self::KeepBoth => "files-clash-keep-both",
            Self::Replace => "files-clash-replace",
        })
    }
}

/// What a click on the sheet decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClashClick {
    Choose(ClashChoice),
    Cancel,
}

/// A copy or move waiting for the person to say what to do about clashes.
#[derive(Clone, Debug)]
pub(crate) struct PendingClash {
    /// Everything to copy or move.
    pub(crate) sources: Vec<PathBuf>,
    /// The sources whose names are taken in `folder`.
    pub(crate) clashing: Vec<PathBuf>,
    pub(crate) folder: PathBuf,
    pub(crate) copy: bool,
    /// For a paste through the desktop's clipboard: the clashing names the
    /// host reported (the first few) and how many there are. The host
    /// carries out the answer.
    pub(crate) clipboard: Option<(Vec<String>, usize)>,
    /// For extracting the archives in `sources`: the names they would take
    /// that `folder` already has. Replace sends those items to Trash first.
    pub(crate) extracting: Option<Vec<String>>,
    hovered: Option<usize>,
    layout: Option<ModalLayout>,
}

/// The sources whose names `folder` already has (a copy into its own folder
/// is a duplicate, not a clash).
pub(crate) fn clashing(sources: &[PathBuf], folder: &Path, copy: bool) -> Vec<PathBuf> {
    sources
        .iter()
        .filter(|source| !(copy && source.parent() == Some(folder)))
        .filter(|source| {
            source
                .file_name()
                .is_some_and(|name| std::fs::symlink_metadata(folder.join(name)).is_ok())
        })
        .cloned()
        .collect()
}

impl PendingClash {
    pub(crate) fn new(
        sources: Vec<PathBuf>,
        clashing: Vec<PathBuf>,
        folder: PathBuf,
        copy: bool,
    ) -> Self {
        Self {
            sources,
            clashing,
            folder,
            copy,
            clipboard: None,
            extracting: None,
            hovered: None,
            layout: None,
        }
    }

    /// A clipboard paste into `folder` waiting for the answer.
    pub(crate) fn clipboard(names: Vec<String>, count: usize, folder: PathBuf) -> Self {
        Self {
            clipboard: Some((names, count)),
            ..Self::new(Vec::new(), Vec::new(), folder, true)
        }
    }

    /// Extracting `archives` into `folder`, where `clashing` (an archive and
    /// the name it would take) are in use.
    pub(crate) fn extraction(
        archives: Vec<PathBuf>,
        clashing: Vec<(PathBuf, String)>,
        folder: PathBuf,
    ) -> Self {
        let (clashing, names) = clashing.into_iter().unzip();
        Self {
            extracting: Some(names),
            ..Self::new(archives, clashing, folder, true)
        }
    }

    /// The existing items a Replace sends to Trash.
    pub(crate) fn replaced(&self) -> Vec<PathBuf> {
        self.clashing
            .iter()
            .filter_map(|source| Some(self.folder.join(source.file_name()?)))
            .collect()
    }

    /// The sources a Skip still copies or moves.
    pub(crate) fn unclashing(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .filter(|source| !self.clashing.contains(source))
            .cloned()
            .collect()
    }

    pub(crate) fn title(&self) -> String {
        let folder = name_of(&self.folder);
        let (first, count) = match (&self.clipboard, &self.extracting) {
            (Some((names, count)), _) => (names.first().cloned(), *count),
            (None, Some(names)) => (names.first().cloned(), names.len()),
            (None, None) => (
                self.clashing.first().map(|path| name_of(path)),
                self.clashing.len(),
            ),
        };
        match (count, first) {
            (1, Some(name)) => t_args(
                "files-clash-title-one",
                &[arg("name", name), arg("folder", folder)],
            ),
            (count, _) => t_args(
                "files-clash-title-many",
                &[
                    arg("count", ui::num::i64_of_usize(count)),
                    arg("folder", folder),
                ],
            ),
        }
    }

    pub(crate) fn detail() -> String {
        t("files-clash-detail")
    }

    pub(crate) fn choice_labels() -> Vec<String> {
        ClashChoice::ALL
            .iter()
            .map(|choice| choice.label())
            .collect()
    }

    /// Paint the sheet over `window`.
    pub(crate) fn paint(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        window: Rect,
    ) {
        let title = self.title();
        let detail = Self::detail();
        let labels = Self::choice_labels();
        let actions: Vec<ModalAction<'_>> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let action = if i + 1 == labels.len() {
                    ModalAction::danger(label)
                } else {
                    ModalAction::secondary(label)
                };
                action.hovered(self.hovered == Some(i))
            })
            .collect();
        let body_width =
            modal_dialog::body_width(window, modal_dialog::MODAL_MAX_WIDTH, theme) as f32;
        let style = theme.text(ui::TextRole::Body);
        let (_, body_h) = text_renderer.measure_style(&detail, Some(body_width), &style);
        let spec = ModalSpec::new(&title)
            .actions(&actions)
            .body_height(f64::from(body_h));
        let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
        modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
        let (_, dim) = modal_dialog::ink(theme);
        text_renderer.draw_style(
            scene,
            &detail,
            dim,
            transform * vello::kurbo::Affine::translate((layout.body.x0, layout.body.y0)),
            Some(layout.body.width() as f32),
            &style,
        );
        modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        self.layout = Some(layout);
    }

    /// Follow the pointer; whether the hovered button changed.
    pub(crate) fn pointer_moved(&mut self, point: Point) -> bool {
        let hovered = match self.layout.as_ref().and_then(|layout| layout.hit(point)) {
            Some(ModalHit::Action(i)) => Some(i),
            _ => None,
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    /// What a click at `point` decides, if anything.
    pub(crate) fn click(&self, point: Point) -> Option<ClashClick> {
        let layout = self.layout.as_ref()?;
        match layout.hit(point) {
            Some(ModalHit::Action(i)) => ClashChoice::ALL.get(i).copied().map(ClashClick::Choose),
            Some(ModalHit::Close) => Some(ClashClick::Cancel),
            _ if layout.dismisses(point) => Some(ClashClick::Cancel),
            _ => None,
        }
    }

    /// The choice an action target names, for assistive technology.
    pub(crate) fn choice_at(index: usize) -> Option<ClashChoice> {
        ClashChoice::ALL.get(index).copied()
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clashes_are_names_taken_in_the_folder_but_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(src.join(name), b"new").unwrap();
        }
        std::fs::write(dest.join("a.txt"), b"old").unwrap();
        let sources = vec![src.join("a.txt"), src.join("b.txt")];
        let found = clashing(&sources, &dest, false);
        assert_eq!(found, vec![src.join("a.txt")]);
        assert!(
            clashing(&sources, &src, true).is_empty(),
            "a copy into its own folder is a duplicate"
        );
        let pending = PendingClash::new(sources, found, dest.clone(), false);
        assert_eq!(pending.replaced(), vec![dest.join("a.txt")]);
        assert_eq!(pending.unclashing(), vec![src.join("b.txt")]);
        assert!(pending.title().contains("a.txt"), "{}", pending.title());
    }
}
