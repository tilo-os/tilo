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
use ui::tilo_resource::{self, NonBlank};

use crate::model::Tracker;

/// The app that keeps the workspace's notes.
pub const NOTES: &str = "so.tilo.notes";
/// Its notebook, the resource that makes a note.
const NOTEBOOK: &str = "notes.notebook";
/// The kind of a note, an item of the notebook's pages.
pub(crate) const NOTE_KIND: &str = "notes.notebook.pages";
/// The notebook's action that makes a note with a title.
const NEW_NOTE: &str = "create-page";

/// What Tasks does with other apps' resources.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "tasks.effect")]
pub enum TasksEffect {
    /// Start a project
    ///
    /// Open the form a project or an area is made in. It changes nothing on
    /// its own: a script or an agent that means to make one invokes the
    /// tracker's own action with a name. It edits nothing, so a viewer may
    /// open it too.
    #[effect(kind = "read")]
    NewProject,

    /// Open issue
    ///
    /// Show an issue's page, as choosing it in the list does.
    #[effect(view, idempotent, item = issues)]
    #[effect(icon = "circle-dot")]
    OpenIssue {
        #[param("The issue")]
        issue: Id,
    },

    /// Open as chat
    ///
    /// Show an issue as a conversation with its agent, in a window of its
    /// own: the thread, the run's steps as they come, what it asks, and a
    /// field to tell it more.
    #[effect(view, idempotent, item = issues)]
    #[effect(icon = "message-circle")]
    OpenChat {
        #[param("The issue")]
        issue: Id,
    },

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
        // The window answers these itself as they arrive (see `lib::router`):
        // the form a project is made in, the page an issue is shown on, and
        // the window an issue's chat is.
        TasksEffect::NewProject | TasksEffect::OpenIssue { .. } | TasksEffect::OpenChat { .. } => {
            Ok(Effected::done())
        }
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
