//! Saving the merged result.
//!
//! The tool's whole reason to exist is to hand a finished merge back, so
//! this is the one thing Diff & Merge writes: where the person pointed the
//! host's chooser, or the place they last saved to. That place is kept here,
//! on this device — it is a fact about here, not about the document.

use std::path::Path;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::portal::PickRequest;

use super::effects::{Asked, Shared};
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

/// Write the merged result: to `path` when the caller names one, else where
/// it was last written, asking for a place when there is none (or when `ask`
/// is set, as Save as… does).
pub(super) fn answer_save(
    store: &Shared,
    cx: &mut EffectCx<Session>,
    path: Option<ui::tilo_resource::AbsolutePath>,
    ask: bool,
) -> EffectAnswer {
    let Some(merged) = store.borrow().merged() else {
        return Err("no merge is open, so there is no merged result to save".into());
    };
    let bytes = merged.clone().into_bytes();
    let written = Rc::clone(store);
    if let Some(path) = path {
        return Ok(cx.thread(
            move |_progress| write_text(&path, &bytes),
            move |(), result, _| finish_save(&written, result, merged),
        ));
    }
    let saved_place = store.borrow().saved_place.clone();
    if !ask && let Some(place) = saved_place {
        let name = place.name().to_owned();
        return Ok(cx.write_destination(&place, bytes, move |(), result, _| {
            finish_save(&written, result.map(|()| name), merged)
        }));
    }
    let suggested = suggested_save_name(store);
    Ok(
        cx.choose_destination(save_request(&suggested), move |(), place, cx| {
            let Some(place) = place else {
                // Asking and being answered with "no" is not a failure of the
                // tool; the result simply stays unsaved.
                written.borrow_mut().ask(Asked::Notice(t("save-cancelled")));
                return Err("saving the merged result was cancelled".into());
            };
            let name = place.name().to_owned();
            written.borrow_mut().saved_place = Some(place.clone());
            Ok(cx.write_destination(&place, bytes, move |(), result, _| {
                finish_save(&written, result.map(|()| name), merged)
            }))
        }),
    )
}

/// The name a save suggests: the file that was merged, whose result this is,
/// or a plain merged result name.
fn suggested_save_name(store: &Shared) -> String {
    store
        .borrow()
        .live
        .as_ref()
        .and_then(|live| {
            live.get()
                .merge
                .as_ref()
                .map(|merge| merge.ours_label.0.clone())
        })
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "merged.txt".to_owned())
}

/// The save of `merged` finished: `Ok(name)` when the result was written, the
/// reason it was not otherwise. Says so in the status bar either way.
fn finish_save(store: &Shared, result: Result<String, String>, merged: String) -> EffectAnswer {
    let mut store = store.borrow_mut();
    match result {
        Ok(name) => {
            store.saved_text = Some(merged);
            store.ask(Asked::Notice(t_args(
                "save-saved",
                &[("name", text(&name))],
            )));
            Ok(Effected::done())
        }
        Err(reason) => {
            store.ask(Asked::Notice(t_args(
                "save-failed",
                &[("reason", text(&reason))],
            )));
            Err(reason)
        }
    }
}
