//! Writing a note for an issue: a note in Notes, titled after the issue and
//! linked from it.
//!
//! Notes makes the note through its own published action and the tracker
//! links it through its own, both for whoever asked: the person at the
//! palette, an agent working on the issue, another member. Each app records
//! its part once in its own history, where that person's undo takes it
//! back, and the host decides each part with that person's grants. Nothing
//! here knows how Notes keeps a note.
//!
//! The effect vocabulary is the record's ([`crate::model::TasksEffect`]),
//! shared with whatever else reads the tracker; this module is where Tasks
//! answers it.

use serde_json::json;
use ui::framework::{Call, EffectAnswer, EffectCx, Effected};
use ui::prelude::*;
use ui::tilo_reference::ResourceRef;
use ui::tilo_resource::{self, NonBlank};

pub use crate::model::TasksEffect;
use crate::model::{NOTES, Tracker};

/// Its notebook, the resource that makes a note.
const NOTEBOOK: &str = "notes.notebook";
/// The notebook's action that makes a note with a title.
const NEW_NOTE: &str = "create-page";

/// Answer `effect`.
pub(crate) fn handle(effect: TasksEffect, cx: &mut EffectCx<Tracker>) -> EffectAnswer {
    match effect {
        TasksEffect::WriteNote { issue } => write_note(cx, issue),
        // The window answers these itself as they arrive (see `lib::router`):
        // the form a project is made in, the page an issue is shown on, and
        // the place showing, and the window an issue's chat is.
        TasksEffect::NewProject
        | TasksEffect::OpenIssue { .. }
        | TasksEffect::OpenChat { .. }
        | TasksEffect::ShowList
        | TasksEffect::ShowBoard
        | TasksEffect::ShowTasks
        | TasksEffect::ShowThread => Ok(Effected::done()),
    }
}

fn write_note(cx: &mut EffectCx<Tracker>, issue: Id) -> EffectAnswer {
    let title = cx
        .live()
        .with(|tracker| {
            tracker
                .issues
                .get(&issue)
                .map(|found| format!("{} {}", crate::reference(found.number), found.title))
        })
        .ok_or_else(|| "There is no such issue".to_owned())?;
    let notebook = ResourceRef::whole(NOTEBOOK, NOTEBOOK, crate::tr("tasks-notebook"));
    let note = Call::on(NOTES, None, &notebook, NEW_NOTE).with(json!({ "title": title }));
    Ok(cx.call(&note, move |(), made, cx| {
        let made = made?;
        let note = made
            .resources
            .first()
            .cloned()
            .ok_or_else(|| "Notes made no note".to_owned())?;
        let kind = tilo_resource::items::item_kind(&note)
            .ok_or_else(|| format!("`{note}` names no note"))?;
        let app = NonBlank::parse(NOTES).ok_or_else(|| "Notes has no app id".to_owned())?;
        let resource = NonBlank::parse(&note).ok_or_else(|| format!("`{note}` names no note"))?;
        cx.submit(Tracker::LINK.with((issue, app, kind, resource, title, None::<String>)));
        cx.when_landed(move |(), _, _| Ok(Effected::value(json!({ "resource": note }))))
    }))
}
