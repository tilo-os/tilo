//! Name clashes when copying, moving or extracting: one question, asked over the
//! window, for every clashing name.

use std::path::{Path, PathBuf};

use crate::i18n::{arg, t, t_args};
use ui::widget::modal_dialog::{self, ModalAction};
use ui::widget::{ModalHit, ModalLayout, ModalSpec};
use vello::kurbo::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClashChoice {
    Skip,
    KeepBoth,
    Replace,
}

impl ClashChoice {
    const ALL: [Self; 3] = [Self::Skip, Self::KeepBoth, Self::Replace];

    fn label(self) -> String {
        t(match self {
            Self::Skip => "files-clash-skip",
            Self::KeepBoth => "files-clash-keep-both",
            Self::Replace => "files-clash-replace",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClashClick {
    Choose(ClashChoice),
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingClash {
    pub(crate) sources: Vec<PathBuf>,
    pub(crate) clashing: Vec<PathBuf>,
    pub(crate) folder: PathBuf,
    pub(crate) copy: bool,
    pub(crate) clipboard: Option<(Vec<String>, usize)>,
    pub(crate) extracting: Option<Vec<String>>,
    hovered: Option<usize>,
    layout: Option<ModalLayout>,
}

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

    pub(crate) fn clipboard(names: Vec<String>, count: usize, folder: PathBuf) -> Self {
        Self {
            clipboard: Some((names, count)),
            ..Self::new(Vec::new(), Vec::new(), folder, true)
        }
    }

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

    pub(crate) fn replaced(&self) -> Vec<PathBuf> {
        self.clashing
            .iter()
            .filter_map(|source| Some(self.folder.join(source.file_name()?)))
            .collect()
    }

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

    pub(crate) fn pointer_moved(&mut self, point: Point) -> bool {
        let hovered = match self.layout.as_ref().and_then(|layout| layout.hit(point)) {
            Some(ModalHit::Action(i)) => Some(i),
            _ => None,
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    pub(crate) fn click(&self, point: Point) -> Option<ClashClick> {
        let layout = self.layout.as_ref()?;
        match layout.hit(point) {
            Some(ModalHit::Action(i)) => ClashChoice::ALL.get(i).copied().map(ClashClick::Choose),
            Some(ModalHit::Close) => Some(ClashClick::Cancel),
            _ if layout.dismisses(point) => Some(ClashClick::Cancel),
            _ => None,
        }
    }

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
