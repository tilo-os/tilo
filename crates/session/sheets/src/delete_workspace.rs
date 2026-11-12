//! "Delete \<name\>?": the one question asked before a workspace of the
//! person's own, held by no other device of theirs, goes with everything in
//! it.
//!
//! Deleting is destructive, so it is a question and not a notice: a small
//! dialog over what the person was looking at, with the safe answer under the
//! keyboard — Enter and Escape keep the workspace, as does a click beside the
//! dialog — and the red destructive button taking a press of its own. The
//! shell opens it wherever deleting was asked for (the workspace menu, the
//! settings, an agent's action) and carries the answer out through the desk's
//! own action, which is the only thing that deletes.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{Column, Element, ModalDialog, Text};

use crate::presentation::Presentation;
use crate::t;

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.delete-workspace";

/// The sheet's resource: the door its answers are published on. It holds
/// nothing; the question is the shell's, which is what answers it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DeleteWorkspace {}

#[resource(kind = "delete_workspace.question")]
impl DeleteWorkspace {}

/// The question the sheet asks: the workspace's name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    pub name: String,
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.delete-workspace.answer")]
pub enum DeleteWorkspaceEffect {
    /// Delete it, and everything in it
    #[effect(
        own_device,
        icon = "trash",
        keywords = "delete, remove, löschen, entfernen"
    )]
    Delete,
    /// Keep it
    #[effect(
        own_device,
        kind = "read",
        icon = "x",
        keywords = "keep, cancel, behalten, abbrechen"
    )]
    Keep,
}

/// What the shell lends the sheet: which workspace is asked about, and what
/// the person answered.
#[derive(Clone, Default)]
pub struct DeleteWorkspaceSeat {
    presentation: Presentation,
    asked: Rc<RefCell<Asked>>,
    shown: Rc<RefCell<Option<LocalState<Asked>>>>,
    answers: Rc<RefCell<Vec<DeleteWorkspaceEffect>>>,
}

impl std::fmt::Debug for DeleteWorkspaceSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeleteWorkspaceSeat")
            .field("asked", &self.asked.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for DeleteWorkspaceSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl DeleteWorkspaceSeat {
    /// Ask about the workspace called `name`.
    pub fn show(&self, name: String) {
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| shown.name.clone_from(&name));
        }
        *self.asked.borrow_mut() = Asked { name };
    }

    /// The workspace asked about now.
    #[must_use]
    pub fn asked(&self) -> Asked {
        self.asked.borrow().clone()
    }

    /// The answers made since the shell last asked, oldest first.
    pub fn take_answers(&self) -> Vec<DeleteWorkspaceEffect> {
        std::mem::take(&mut *self.answers.borrow_mut())
    }

    /// Answer as the sheet does, for a caller that answers in its place: a
    /// test or a capture, which never presses the button.
    #[doc(hidden)]
    pub fn answer(&self, effect: DeleteWorkspaceEffect) {
        self.answers.borrow_mut().push(effect);
    }
}

/// The sheet as an app.
pub struct DeleteWorkspaceApp;

impl ResourceApp for DeleteWorkspaceApp {
    type Resource = DeleteWorkspace;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Delete workspace";
    const ICON: Option<&'static str> = Some("trash");
    // Over the desktop: what is not the sheet shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<DeleteWorkspace>) -> Element {
        let seat = cx.take_lent::<DeleteWorkspaceSeat>();
        let initial = match &seat {
            Some(seat) => Some(seat.asked()),
            None => cx.fixture::<Asked>().map(|asked| (*asked).clone()),
        };
        let shown = cx.local(initial.unwrap_or_default());
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
        }
        let answering = seat;
        cx.effects(move |effect: DeleteWorkspaceEffect, _cx| match &answering {
            Some(seat) => {
                seat.answers.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("there is no workspace to delete here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        cx.switch(&shown, move |_cx, asked| card(asked, &dispatch))
    }

    fn scenarios() -> &'static [&'static str] {
        &["named"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<DeleteWorkspace>) {
        fixture.provide(Asked {
            name: "Design studio".to_owned(),
        });
    }
}

/// The question about the workspace called `asked.name`: what goes with it,
/// and the two answers.
fn card(asked: &Asked, dispatch: &Dispatcher<DeleteWorkspace>) -> Element {
    let name = [("name", ui::i18n::ArgValue::Text(asked.name.clone()))];
    let body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(Text::new(&crate::t_args(
            "workspace-delete-body",
            &name,
        ))));
    let deleting = dispatch.clone();
    let keeping = dispatch.clone();
    let dismissed = dispatch.clone();
    Box::new(
        ModalDialog::new(crate::t_args("workspace-delete-title", &name), body)
            .with_test_id("workspace-delete")
            .with_icon(ui::icons::Icon::Trash2)
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .open(true)
            .with_destructive(true)
            .primary_action(t("workspace-delete-confirm"), move || {
                deleting.effect(&DeleteWorkspaceEffect::Delete);
            })
            .secondary_action(t("workspace-delete-keep"), move || {
                keeping.effect(&DeleteWorkspaceEffect::Keep);
            })
            .on_dismiss(move || dismissed.effect(&DeleteWorkspaceEffect::Keep)),
    )
}

/// The question's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DeleteWorkspaceApp)
}
