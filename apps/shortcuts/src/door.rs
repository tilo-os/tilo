//! The app's half of the action door: the saved library, the catalogue
//! steps are chosen from, saving, running and deleting.
//!
//! Everything here goes through the session broker as the Shortcuts app
//! ([`ui::RemoteActions`]). Reading the catalogue is the manifest's
//! `[actions] query`; the library (`shortcuts#list|create|update|delete`)
//! and each saved shortcut's `run` are published under this app's own id,
//! which the broker lets it use as the person using it. Running goes
//! through `shortcuts.shortcut:<id>#run`, so the session executes the steps
//! as that person within the shortcut's grant — never this app.
//!
//! Each operation is also an effect the app publishes on its drafts
//! ([`DoorEffect`]), so the palette, an agent or a script saves or runs a
//! shortcut through the same code the buttons use.

use std::cell::RefCell;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use serde_json::{Map, Value, json};
use tilo_automation::{ActionCatalog, SHORTCUTS_APP_ID, Shortcut, shortcut_resource_id};
use ui::framework::{EffectReply, Live};
use ui::prelude::*;
use ui::tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, MAX_CATALOGUE_PAGE, RemoteActionTarget,
};
use ui::tilo_actions::{ActionOutput, InvocationUpdate};
use ui::widget::Decorator;
use ui::{CatalogueCall, InvocationCall, LocalState, RemoteActions, RemoteCall};

use crate::editor;
use crate::model::Workbench;

/// What the door does, published on the drafts for every caller.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "shortcuts.door", device_only)]
pub enum DoorEffect {
    /// Reload shortcuts.
    #[effect(job)]
    Refresh,
    /// Save shortcut.
    #[effect(job)]
    Save {
        #[param("Id of the draft to save")]
        draft: String,
    },
    /// Run shortcut.
    #[effect(job)]
    Run {
        #[param("Id of the saved shortcut")]
        shortcut: String,
        #[param("Values for the shortcut's inputs")]
        inputs: Option<Map<String, Value>>,
    },
    /// Delete shortcut.
    #[effect(job, destructive)]
    Delete {
        #[param("Id of the saved shortcut")]
        shortcut: String,
    },
    /// Draft a shortcut with the agent.
    #[effect(job)]
    Describe {
        #[param("What the shortcut should do, in your words")]
        description: String,
    },
}

/// The library's own actions, published by the session under this app's id.
const LIBRARY_RESOURCE: &str = "shortcuts";

/// Something the door reads, as it stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Loaded<T> {
    #[default]
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loaded<T> {
    #[must_use]
    pub const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Loading | Self::Failed(_) => None,
        }
    }
}

/// Where one run stands.
#[derive(Clone, Debug, PartialEq)]
pub enum RunStatus {
    Running,
    Completed(Option<Value>),
    Failed(String),
}

/// One run started from this app, newest first in [`DoorState::runs`].
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub serial: u64,
    pub shortcut_id: String,
    pub name: String,
    pub status: RunStatus,
    /// When the run started, as the person reads the clock.
    pub started: String,
}

/// The outcome of the last save or delete, for the person to read.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Saved(String),
    Deleted(String),
    Failed(String),
}

/// Where asking the agent for a draft stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Drafting {
    Drafting,
    Ready(Box<crate::drafting::Drafted>),
    Failed(String),
}

/// Everything the door knows, shown by the views.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoorState {
    pub catalogue: Loaded<ActionCatalog>,
    pub library: Loaded<Vec<Shortcut>>,
    pub runs: Vec<RunRecord>,
    pub notice: Option<Notice>,
    /// The last draft asked of the agent.
    pub drafted: Option<Drafting>,
}

impl DoorState {
    /// The saved shortcut `id`, when the library holds it.
    #[must_use]
    pub fn saved(&self, id: &str) -> Option<&Shortcut> {
        self.library
            .ready()
            .and_then(|library| library.iter().find(|shortcut| shortcut.id == id))
    }

    /// The catalogue as far as it is known; empty until it is read.
    #[must_use]
    pub fn catalog(&self) -> ActionCatalog {
        self.catalogue.ready().cloned().unwrap_or_default()
    }

    /// The last run of shortcut `id`.
    #[must_use]
    pub fn last_run(&self, id: &str) -> Option<&RunRecord> {
        self.runs.iter().find(|run| run.shortcut_id == id)
    }
}

type Reply = EffectReply;

enum Purpose {
    Catalogue,
    List,
    Save { name: String },
    Run { serial: u64 },
    Delete { name: String },
    Draft,
}

enum Answer {
    Catalogue(LocalState<CatalogueCall>),
    Invocation(LocalState<InvocationCall>),
}

struct Call {
    purpose: Purpose,
    answer: Answer,
    reply: Option<Reply>,
}

/// The door's working state, shared by the effect handler and the widget
/// that pumps it.
pub struct DoorCore {
    remote: RemoteActions,
    state: LocalState<DoorState>,
    live: Live<Workbench>,
    calls: Vec<Call>,
    next_run: u64,
    /// Lists still being read for the current refresh, and what they found.
    listing: Vec<Shortcut>,
}

impl DoorCore {
    #[must_use]
    pub fn new(remote: RemoteActions, state: LocalState<DoorState>, live: Live<Workbench>) -> Self {
        Self {
            remote,
            state,
            live,
            calls: Vec::new(),
            next_run: 1,
            listing: Vec::new(),
        }
    }

    fn row_target(&self, resource: &str, action: &str) -> Option<RemoteActionTarget> {
        self.state.with(|state| {
            state.catalogue.ready().and_then(|catalog| {
                catalog
                    .offering(SHORTCUTS_APP_ID, action)
                    .find(|row| row.resource.id == resource)
                    .and_then(ui::tilo_actions::remote::CatalogueRow::target)
            })
        })
    }

    /// Why nothing that needs the catalogue can be done yet, in the
    /// person's words.
    fn catalogue_missing(&self) -> String {
        self.state.with(|state| match &state.catalogue {
            Loaded::Failed(_) => {
                "the actions your apps offer could not be read here, so nothing can be saved"
                    .to_owned()
            }
            Loaded::Loading | Loaded::Ready(_) => {
                "the actions your apps offer are still being read; try again in a moment".to_owned()
            }
        })
    }

    /// Read the catalogue, then the library it offers.
    pub fn refresh(&mut self, reply: Option<Reply>) {
        let answer = self.remote.catalogue(CatalogueQuery {
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        });
        self.state.update(|state| {
            if state.catalogue.ready().is_none() {
                state.catalogue = Loaded::Loading;
            }
        });
        self.calls.push(Call {
            purpose: Purpose::Catalogue,
            answer: Answer::Catalogue(answer),
            reply,
        });
    }

    fn invoke(
        &mut self,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
        purpose: Purpose,
        reply: Option<Reply>,
    ) {
        let argument = if arguments.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(arguments)
        };
        let answer = self.remote.invoke(target, argument, None);
        self.calls.push(Call {
            purpose,
            answer: Answer::Invocation(answer),
            reply,
        });
    }

    /// Save draft `draft` with the least grant its steps need, after the
    /// same check the editor shows.
    ///
    /// # Errors
    ///
    /// Why the draft cannot be saved now; nothing was sent.
    pub fn save(&mut self, draft: &str, reply: Option<Reply>) -> Result<(), String> {
        let id = Id::from_raw(draft);
        let draft = self
            .live
            .with(|workbench| workbench.drafts.get(&id).cloned())
            .ok_or("there is no such draft")?;
        let (catalog, saved) = self.state.with(|state| {
            (
                state.catalogue.ready().cloned(),
                state.library.ready().cloned().unwrap_or_default(),
            )
        });
        if draft.program().steps.is_empty() {
            return Err("add a step first: an empty shortcut does nothing".to_owned());
        }
        let catalog = catalog.ok_or_else(|| self.catalogue_missing())?;
        self.state.update(|state| state.notice = None);
        let problems = editor::problems(&draft, &catalog, &saved);
        if let Some(first) = editor::problem_lines(&draft.program().steps, &problems).first() {
            return Err(first.clone());
        }
        let shortcut = editor::candidate(&draft, &catalog);
        let action = if saved.iter().any(|other| other.id == shortcut.id) {
            "update"
        } else {
            "create"
        };
        let target = self
            .row_target(LIBRARY_RESOURCE, action)
            .ok_or("this workspace offers no shortcut library")?;
        let value = serde_json::to_value(&shortcut).map_err(|error| error.to_string())?;
        let arguments = Map::from_iter([("shortcut".to_owned(), value)]);
        self.invoke(
            target,
            arguments,
            Purpose::Save {
                name: shortcut.name,
            },
            reply,
        );
        Ok(())
    }

    /// Run saved shortcut `shortcut` with `inputs`.
    ///
    /// # Errors
    ///
    /// Why it cannot be run now; nothing was sent.
    pub fn run(
        &mut self,
        shortcut: &str,
        inputs: Map<String, Value>,
        reply: Option<Reply>,
    ) -> Result<(), String> {
        let name = self
            .state
            .with(|state| state.saved(shortcut).map(|saved| saved.name.clone()))
            .ok_or_else(|| format!("there is no saved shortcut `{shortcut}`"))?;
        let target = self
            .row_target(
                &shortcut_resource_id(shortcut),
                tilo_automation::SHORTCUT_RUN_ACTION,
            )
            .ok_or_else(|| format!("“{name}” cannot be run here"))?;
        let serial = self.next_run;
        self.next_run = self.next_run.saturating_add(1);
        self.state.update(|state| {
            state.runs.insert(
                0,
                RunRecord {
                    serial,
                    shortcut_id: shortcut.to_owned(),
                    name,
                    status: RunStatus::Running,
                    started: ui::clock::time_of_day(&ui::clock::local_now()),
                },
            );
        });
        self.invoke(target, inputs, Purpose::Run { serial }, reply);
        Ok(())
    }

    /// Ask the agent to draft a shortcut doing what `description` says.
    ///
    /// # Errors
    ///
    /// Why no draft can be asked for now; nothing was sent.
    pub fn describe(&mut self, description: &str, reply: Option<Reply>) -> Result<(), String> {
        let description = description.trim();
        if description.is_empty() {
            return Err("describe what the shortcut should do".to_owned());
        }
        let catalog = self
            .state
            .with(|state| state.catalogue.ready().cloned())
            .ok_or_else(|| self.catalogue_missing())?;
        let target = crate::drafting::target(&catalog)
            .ok_or("the agent does not offer drafting in this workspace")?;
        let arguments = crate::drafting::arguments(description, &catalog);
        self.state
            .update(|state| state.drafted = Some(Drafting::Drafting));
        self.invoke(target, arguments, Purpose::Draft, reply);
        Ok(())
    }

    /// Delete saved shortcut `shortcut`.
    ///
    /// # Errors
    ///
    /// Why it cannot be deleted now; nothing was sent.
    pub fn delete(&mut self, shortcut: &str, reply: Option<Reply>) -> Result<(), String> {
        let name = self
            .state
            .with(|state| state.saved(shortcut).map(|saved| saved.name.clone()))
            .ok_or_else(|| format!("there is no saved shortcut `{shortcut}`"))?;
        let target = self
            .row_target(LIBRARY_RESOURCE, "delete")
            .ok_or("this workspace offers no shortcut library")?;
        let arguments = Map::from_iter([("id".to_owned(), Value::String(shortcut.to_owned()))]);
        self.invoke(target, arguments, Purpose::Delete { name }, reply);
        Ok(())
    }

    /// Settle every answer that has arrived.
    pub fn pump(&mut self) {
        let mut index = 0;
        while index < self.calls.len() {
            let settled = self
                .calls
                .get(index)
                .is_some_and(|call| match &call.answer {
                    Answer::Catalogue(answer) => !answer.get().is_pending(),
                    Answer::Invocation(answer) => answer.get().is_settled(),
                });
            if settled {
                let call = self.calls.remove(index);
                self.settle(call);
            } else {
                index += 1;
            }
        }
    }

    fn settle(&mut self, call: Call) {
        let outcome: Result<Option<ActionOutput>, String> = match &call.answer {
            Answer::Catalogue(answer) => match answer.get() {
                RemoteCall::Done(page) => {
                    self.state.update(|state| {
                        state.catalogue = Loaded::Ready(ActionCatalog::new(page.rows));
                    });
                    self.list_library();
                    Ok(None)
                }
                RemoteCall::Refused(error) => Err(catalogue_refusal(&error)),
                RemoteCall::Pending => return,
            },
            Answer::Invocation(answer) => match answer.get() {
                RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => Ok(Some(output)),
                RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => Err(error.message),
                RemoteCall::Done(InvocationUpdate::Cancelled { message, .. }) => {
                    Err(message.unwrap_or_else(|| "cancelled".to_owned()))
                }
                RemoteCall::Refused(error) => Err(error.to_string()),
                RemoteCall::Done(_) | RemoteCall::Pending => return,
            },
        };
        match (&call.purpose, &outcome) {
            (Purpose::Catalogue, Err(reason)) => self.state.update(|state| {
                state.catalogue = Loaded::Failed(reason.clone());
                state.library = Loaded::Failed(reason.clone());
            }),
            (Purpose::Catalogue, Ok(_)) => {}
            (Purpose::List, Ok(output)) => {
                let found: Vec<Shortcut> = output
                    .as_ref()
                    .and_then(|output| output.value.clone())
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_default();
                self.listing.extend(found);
                if !self
                    .calls
                    .iter()
                    .any(|other| matches!(other.purpose, Purpose::List))
                {
                    let mut listing = std::mem::take(&mut self.listing);
                    listing.sort_by(|left, right| left.name.cmp(&right.name));
                    self.state
                        .update(|state| state.library = Loaded::Ready(listing));
                }
            }
            (Purpose::List, Err(reason)) => {
                self.state
                    .update(|state| state.library = Loaded::Failed(reason.clone()));
            }
            (Purpose::Save { name }, result) => {
                let notice = match result {
                    Ok(_) => Notice::Saved(name.clone()),
                    Err(reason) => Notice::Failed(reason.clone()),
                };
                self.state.update(|state| state.notice = Some(notice));
                if result.is_ok() {
                    self.refresh(None);
                }
            }
            (Purpose::Delete { name }, result) => {
                let notice = match result {
                    Ok(_) => Notice::Deleted(name.clone()),
                    Err(reason) => Notice::Failed(reason.clone()),
                };
                self.state.update(|state| state.notice = Some(notice));
                if result.is_ok() {
                    self.refresh(None);
                }
            }
            (Purpose::Draft, result) => {
                self.settle_draft(result.clone(), call.reply);
                return;
            }
            (Purpose::Run { serial }, result) => {
                let status = match result {
                    Ok(output) => RunStatus::Completed(
                        output.as_ref().and_then(|output| output.value.clone()),
                    ),
                    Err(reason) => RunStatus::Failed(reason.clone()),
                };
                self.state.update(|state| {
                    if let Some(run) = state.runs.iter_mut().find(|run| run.serial == *serial) {
                        run.status = status;
                    }
                });
            }
        }
        if let Some(reply) = call.reply {
            reply.finish(outcome.map(Option::unwrap_or_default));
        }
    }

    /// Offer what the agent drafted, and answer the `describe` caller with
    /// it and the checker's verdict.
    fn settle_draft(&self, result: Result<Option<ActionOutput>, String>, reply: Option<Reply>) {
        let saved = self
            .state
            .with(|state| state.library.ready().cloned().unwrap_or_default());
        let catalog = self.state.with(DoorState::catalog);
        let drafted = result.and_then(|output| {
            let value = output
                .and_then(|output| output.value)
                .ok_or("the agent answered nothing")?;
            crate::drafting::adopt(value, &saved, &catalog)
        });
        let answer = drafted.as_ref().map_err(Clone::clone).and_then(|drafted| {
            serde_json::to_value(&drafted.shortcut)
                .map(|shortcut| ActionOutput {
                    value: Some(json!({
                        "shortcut": shortcut,
                        "problems": drafted.problems,
                    })),
                    resources: Vec::new(),
                })
                .map_err(|error| error.to_string())
        });
        self.state.update(|state| {
            state.drafted = Some(match drafted {
                Ok(drafted) => Drafting::Ready(Box::new(drafted)),
                Err(reason) => Drafting::Failed(reason),
            });
        });
        if let Some(reply) = reply {
            reply.finish(answer);
        }
    }

    fn list_library(&mut self) {
        let targets: Vec<RemoteActionTarget> = self.state.with(|state| {
            state
                .catalogue
                .ready()
                .map(|catalog| {
                    catalog
                        .offering(SHORTCUTS_APP_ID, "list")
                        .filter(|row| row.resource.id == LIBRARY_RESOURCE)
                        .filter_map(ui::tilo_actions::remote::CatalogueRow::target)
                        .collect()
                })
                .unwrap_or_default()
        });
        if targets.is_empty() {
            self.state.update(|state| {
                state.library =
                    Loaded::Failed("this workspace offers no shortcut library".to_owned());
            });
            return;
        }
        self.listing.clear();
        for target in targets {
            self.invoke(target, Map::new(), Purpose::List, None);
        }
    }

    /// Carry out one door effect, answering `reply` once it settled.
    ///
    /// # Errors
    ///
    /// Why the effect was refused before anything was sent.
    pub fn perform(&mut self, effect: DoorEffect, reply: Reply) -> Result<(), String> {
        let reply = Some(reply);
        match effect {
            DoorEffect::Refresh => self.refresh(reply),
            DoorEffect::Save { draft } => self.save(&draft, reply)?,
            DoorEffect::Run { shortcut, inputs } => {
                self.run(&shortcut, inputs.unwrap_or_default(), reply)?;
            }
            DoorEffect::Delete { shortcut } => self.delete(&shortcut, reply)?,
            DoorEffect::Describe { description } => self.describe(&description, reply)?,
        }
        Ok(())
    }
}

/// Hosts the view, feeds the door's requests to the session and settles its
/// answers every frame.
pub struct Door {
    content: Element,
    core: Rc<RefCell<DoorCore>>,
    remote: RemoteActions,
    follow: Option<Box<dyn FnMut()>>,
}

impl Door {
    #[must_use]
    pub fn new(content: Element, core: Rc<RefCell<DoorCore>>) -> Self {
        let remote = core.borrow().remote.clone();
        Self {
            content,
            core,
            remote,
            follow: None,
        }
    }

    /// Run `follow` every frame once the door's answers are settled, to
    /// derive view state from them.
    #[must_use]
    pub fn following(mut self, follow: Box<dyn FnMut()>) -> Self {
        self.follow = Some(follow);
        self
    }
}

impl Decorator for Door {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.remote.poll();
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.pump();
        }
        if let Some(follow) = self.follow.as_mut() {
            follow();
        }
        self.content.update(state, path)
    }

    /// The door is the session's collaborative endpoint; every other request
    /// (the shell's header row, which hosts the window's History control)
    /// passes to the hosted view.
    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn Collaborative>() {
            request.provide::<dyn Collaborative>(&mut self.remote);
        } else {
            self.content.provide_mut(request);
        }
    }

    fn name(&self) -> &'static str {
        "ShortcutsDoor"
    }
}

/// Why the catalogue could not be read, as the person reads it.
pub(crate) fn catalogue_refusal(error: &ui::tilo_actions::remote::ActionHostError) -> String {
    match error {
        ui::tilo_actions::remote::ActionHostError::Unavailable { reason }
            if reason == ui::remote_actions::CATALOGUE_UNANSWERED =>
        {
            "Nothing in this workspace answered when Shortcuts asked which actions your \
             apps offer, so shortcuts can't be read, saved or run here right now."
                .to_owned()
        }
        other => other.to_string(),
    }
}
