//! Saving the merged result.
//!
//! The tool's whole reason to exist is to hand a finished merge back, so
//! this is the one thing Diff & Merge writes: where the person pointed the
//! host's chooser, or the place they last saved to. That place is kept here,
//! on this device — it is a fact about here, not about the document.

use std::path::Path;

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::portal::PickRequest;

use crate::app::DiffMergeApp;
use crate::doc::Session;
use crate::i18n::{t, t_args, text};

/// Ask where to write the merged result, suggesting `name`.
fn save_request(name: &str) -> PickRequest {
    PickRequest::save(&t("save-title"), &t("save-accept"), name)
        .all_files()
        .thread_name("diffmerge-portal-save")
}

/// Write `bytes` to `path`, returning the file name for the status line.
fn write_text(path: &Path, bytes: &[u8]) -> Result<String, String> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    tilo_files::file_at(path)
        .and_then(|(dir, file)| tilo_files::blocking::save_document(&dir, &file, bytes))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(name)
}

impl DiffMergeApp {
    /// Write the merged result: to `path` when the caller names one, else
    /// where it was last written, asking for a place when there is none (or
    /// when `ask` is set, as Save as… does).
    pub(super) fn answer_save(
        &mut self,
        cx: &mut EffectCx<Session, Self>,
        path: Option<String>,
        ask: bool,
    ) -> EffectAnswer {
        if !self.merge_open {
            return Err("no merge is open, so there is no merged result to save".into());
        }
        let bytes = self.merged_text.clone().into_bytes();
        if let Some(path) = path {
            return Ok(cx.thread(
                move |_progress| write_text(Path::new(&path), &bytes),
                |app: &mut Self, written, _| app.finish_save(written),
            ));
        }
        if !ask && let Some(place) = self.saved_place.clone() {
            let name = place.name().to_owned();
            return Ok(cx.write_destination(&place, bytes, move |app, written, _| {
                app.finish_save(written.map(|()| name))
            }));
        }
        let suggested = self.suggested_save_name();
        Ok(cx.choose_destination(
            save_request(&suggested),
            move |app: &mut Self, place, cx| {
                let Some(place) = place else {
                    // Asking and being answered with "no" is not a failure
                    // of the tool; the result simply stays unsaved.
                    app.notice = Some(t("save-cancelled"));
                    app.invalidation.paint = true;
                    return Err("saving the merged result was cancelled".into());
                };
                let name = place.name().to_owned();
                app.saved_place = Some(place.clone());
                Ok(cx.write_destination(&place, bytes, move |app, written, _| {
                    app.finish_save(written.map(|()| name))
                }))
            },
        ))
    }

    /// The name a save suggests: the file that was merged, whose result this
    /// is, or a plain merged result name.
    fn suggested_save_name(&self) -> String {
        self.presented
            .as_ref()
            .and_then(|(_, merge)| merge.as_ref())
            .map(|merge| merge.ours_label.0.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "merged.txt".to_owned())
    }

    /// The save finished: `Ok(name)` when the result was written, the reason
    /// it was not otherwise. Says so in the status bar either way.
    fn finish_save(&mut self, result: Result<String, String>) -> EffectAnswer {
        self.invalidation.paint = true;
        match result {
            Ok(name) => {
                self.saved_text = Some(self.merged_text.clone());
                self.notice = Some(t_args("save-saved", &[("name", text(&name))]));
                Ok(Effected::done())
            }
            Err(reason) => {
                self.notice = Some(t_args("save-failed", &[("reason", text(&reason))]));
                Err(reason)
            }
        }
    }
}
