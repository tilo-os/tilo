//! Comparing two folders.
//!
//! A folder comparison is a way into comparisons, like the file chooser: the
//! listing of what differs lives here, on this device, and every comparison
//! it opens is in the document. The walk runs off the UI thread, so a folder
//! that turns out to be a whole disk cannot freeze the window.

use std::path::Path;

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::portal::PickRequest;

use crate::actions::DiffmergeEffect;
use crate::app::DiffMergeApp;
use crate::doc::Session;
use crate::i18n::{t, t_args, text};

impl DiffMergeApp {
    /// Compare two folders: with both paths, read them now; with none, ask
    /// for the left one and then the right one through the host's chooser.
    pub(super) fn answer_compare_folders(
        &mut self,
        cx: &mut EffectCx<Session, Self>,
        left: Option<String>,
        right: Option<String>,
    ) -> EffectAnswer {
        match (left, right) {
            (Some(left), Some(right)) => Ok(self.read_folders(cx, left, right)),
            (None, None) => Ok(self.choose_folder(cx, None)),
            _ => Err("left and right folder paths must be supplied together".into()),
        }
    }

    /// Ask for one of the two folders, the left one unless `left` holds it
    /// already, and then for the other.
    fn choose_folder(
        &mut self,
        cx: &mut EffectCx<Session, Self>,
        left: Option<std::path::PathBuf>,
    ) -> Effected {
        let title = t(if left.is_some() {
            "folder-choose-right"
        } else {
            "folder-choose-left"
        });
        let request = PickRequest::directory(&title, &t("folder-choose-accept"))
            .thread_name("diffmerge-portal-folder");
        cx.choose_path(request, move |app: &mut Self, path, cx| {
            let Some(path) = path else {
                app.notice = Some(t("folder-cancelled"));
                app.invalidation.paint = true;
                return Err("choosing a folder was cancelled".into());
            };
            match left {
                None => Ok(app.choose_folder(cx, Some(path))),
                Some(left) => {
                    let left = left.to_string_lossy().into_owned();
                    let right = path.to_string_lossy().into_owned();
                    Ok(app.read_folders(cx, left, right))
                }
            }
        })
    }

    /// Read both trees off the UI thread and show the listing.
    fn read_folders(
        &mut self,
        cx: &mut EffectCx<Session, Self>,
        left: String,
        right: String,
    ) -> Effected {
        cx.thread(
            move |_progress| crate::folder::compare(Path::new(&left), Path::new(&right)),
            |app: &mut Self, result, _| app.finish_folders(result),
        )
    }

    /// The folder walk finished.
    fn finish_folders(
        &mut self,
        result: Result<crate::folder::FolderComparison, String>,
    ) -> EffectAnswer {
        self.invalidation.paint = true;
        match result {
            Ok(comparison) => {
                self.folders = Some(comparison);
                self.showing_folders = true;
                self.folder_selection = 0;
                self.folder_scroll = 0.0;
                self.notice = None;
                self.publish_header();
                Ok(Effected::done())
            }
            Err(reason) => {
                self.notice = Some(t_args("folder-failed", &[("reason", text(&reason))]));
                Err(reason)
            }
        }
    }

    /// Choose the folder entry under review, opening those two files as a
    /// comparison.
    pub(super) fn open_folder_entry(&mut self, index: usize) {
        let Some(comparison) = &self.folders else {
            return;
        };
        let Some(entry) = comparison.entries.get(index) else {
            return;
        };
        let (left, right) = comparison.paths(entry);
        match (left, right) {
            (Some(left), Some(right)) => {
                self.effect(&DiffmergeEffect::OpenFiles {
                    left: Some(left),
                    right: Some(right),
                });
            }
            // Only one folder has the file: it is shown against an empty
            // side, which is what "added" and "removed" look like.
            (Some(path), None) => self.effect(&DiffmergeEffect::OpenOne {
                path,
                side: "left".into(),
            }),
            (None, Some(path)) => self.effect(&DiffmergeEffect::OpenOne {
                path,
                side: "right".into(),
            }),
            (None, None) => {}
        }
    }

    /// Scroll the listing so the chosen row shows whole.
    pub(super) fn reveal_folder_row(&mut self, index: usize, row_h: f64) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a row index on screen is far below f64's exact integer range"
        )]
        let top = index as f64 * row_h;
        let bottom = top + row_h;
        let height = self.folder_viewport.height();
        if top < self.folder_scroll {
            self.folder_scroll = top;
        } else if bottom > self.folder_scroll + height {
            self.folder_scroll = bottom - height;
        }
        let max = (self.folder_content_height - height).max(0.0);
        self.folder_scroll = self.folder_scroll.clamp(0.0, max);
    }

    /// Move the folder listing's selection by `step` entries.
    pub(super) fn step_folder(&mut self, step: isize) {
        let Some(comparison) = &self.folders else {
            return;
        };
        let count = comparison.entries.len();
        if count == 0 {
            return;
        }
        let next = self
            .folder_selection
            .saturating_add_signed(step)
            .min(count - 1);
        if next != self.folder_selection {
            self.folder_selection = next;
            self.invalidation.paint = true;
        }
    }
}
