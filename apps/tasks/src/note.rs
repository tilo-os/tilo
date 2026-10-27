//! Writing a note for an issue: a note in Notes, titled after the issue and
//! linked from it.
//!
//! Notes makes the note through its own published action and the tracker
//! links it through its own, both for whoever asked: the person at the
//! palette, an agent working on the issue, another member. Each app records
//! its part once in its own history, where that person's undo takes it
//! back, and the host decides each part with that person's grants. Nothing
//! here knows how Notes keeps a note.

use serde_json::json;
use ui::framework::{Call, EffectAnswer, EffectCx, Effected};
use ui::prelude::*;
use ui::tilo_reference::ResourceRef;
use ui::tilo_resource;

use crate::model::Tracker;

/// The app that keeps the workspace's notes.
pub const NOTES: &str = "so.tilo.notes";
/// Its notebook, the resource that makes a note.
const NOTEBOOK: &str = "notes.notebook";
/// The notebook's action that makes a note with a title.
const NEW_NOTE: &str = "create-page";

/// What Tasks does with other apps' resources.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "tasks.effect")]
pub enum TasksEffect {
    /// Write a note
    ///
    /// A note in Notes for the issue, titled after it and linked from it.
    /// Answers the note's resource.
    #[effect(job, kind = "undoable", item = issues)]
    WriteNote {
        #[param("The issue")]
        issue: Id,
    },
}

/// Answer `effect`.
pub(crate) fn handle(effect: TasksEffect, cx: &mut EffectCx<Tracker>) -> EffectAnswer {
    match effect {
        TasksEffect::WriteNote { issue } => write_note(cx, issue),
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
        cx.submit(Tracker::LINK.with((
            issue,
            NOTES.to_owned(),
            kind,
            note.clone(),
            title,
            None::<String>,
        )));
        cx.when_landed(move |(), _, _| Ok(Effected::value(json!({ "resource": note }))))
    }))
}
