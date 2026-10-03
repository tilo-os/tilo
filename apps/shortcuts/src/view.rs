//! The window: the library beside the editor of the selected draft.
//!
//! Every control is bound to a published action — a document action on the
//! drafts, a door effect, or a view action for what is only presentation
//! (which draft is open, whether a sheet shows). The editor is rebuilt when
//! the draft's structure changes and not while a value is typed, so a field
//! keeps its caret; problems and binding summaries follow every edit.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use serde_json::{Value, json};
use tilo_automation::{
    ActionCatalog, AiTransform, Binding, Condition, Shortcut, Step, StepKind, values_at,
};
use ui::framework::{Actions, Live};
use ui::prelude::*;
use ui::tilo_actions::{ActionParam, ParamType};
use ui::widget::{Actions as ActionGroup, ModalDialog, NumberInput};
use ui::{LocalState, RemoteActions};

use crate::door::{self, Door, DoorCore, DoorEffect, DoorState, Loaded, Notice, RunStatus};
use crate::editor::{self, Choice};
use crate::grant::{self, Permission};
use crate::model::{Draft, INPUT_TYPES, Workbench, block_at};

/// Choice value meaning "type a value here".
const LITERAL: &str = "literal";
/// Choice value meaning "not bound".
const UNSET: &str = "unset";

/// What the editor is built from: the open draft and what the door knows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorInput {
    pub draft: Option<(Id, Draft)>,
    pub catalog: ActionCatalog,
    pub catalogue: Loaded<()>,
    pub saved: Vec<Shortcut>,
    pub problems: BTreeMap<String, Vec<String>>,
    pub picker: Option<String>,
    pub sheet: bool,
}

impl EditorInput {
    /// What the built editor depends on: everything but typed values.
    fn structure(&self) -> String {
        let Some((id, draft)) = &self.draft else {
            return format!("none|{:?}|{}", self.catalogue, self.catalog.rows().len());
        };
        let program = &draft.program.0;
        let mut shape = String::new();
        shape_of(&program.steps, &mut shape);
        format!(
            "{}|{}|{:?}|{}|{}|{:?}|{:?}|{}|{}",
            id.as_str(),
            shape,
            program
                .inputs
                .iter()
                .map(|input| (input.name.to_string(), input.ty))
                .collect::<Vec<_>>(),
            program
                .output
                .as_ref()
                .map(shape_of_binding)
                .unwrap_or_default(),
            self.catalog.rows().len(),
            self.catalogue,
            self.picker,
            self.sheet,
            self.saved.iter().any(|saved| saved.id == draft.shortcut_id),
        )
    }
}

fn shape_of_binding(binding: &Binding) -> String {
    match binding {
        Binding::Literal(_) => "literal".into(),
        Binding::Template(_) => "template".into(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn shape_of(steps: &[Step], out: &mut String) {
    for step in steps {
        out.push_str(&step.id);
        out.push('(');
        match &step.kind {
            StepKind::Invoke {
                app_id,
                resource,
                action,
                args,
            } => {
                let _ = write!(out, "{app_id}/{resource:?}/{action}");
                for (name, binding) in args {
                    let _ = write!(out, ";{name}={}", shape_of_binding(binding));
                }
            }
            StepKind::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = match cond {
                    Condition::Truthy(binding) => shape_of_binding(binding),
                    other => format!("{other:?}"),
                };
                let _ = write!(out, "if {cond} then[");
                shape_of(then, out);
                out.push_str("] else[");
                shape_of(otherwise, out);
                out.push(']');
            }
            StepKind::ForEach { items, body, .. } => {
                let _ = write!(out, "each {} [", shape_of_binding(items));
                shape_of(body, out);
                out.push(']');
            }
            StepKind::SetVar { value, .. } => {
                let _ = write!(out, "set {}", shape_of_binding(value));
            }
            StepKind::Ask { param } => {
                let _ = write!(out, "ask {:?}", param.ty);
            }
            StepKind::Ai { transform, input } => {
                let transform = match transform {
                    AiTransform::Summarise => "summarise",
                    AiTransform::Extract { .. } => "extract",
                    AiTransform::Classify { .. } => "classify",
                    AiTransform::Freeform { .. } => "freeform",
                };
                let _ = write!(out, "ai {transform} {}", shape_of_binding(input));
            }
        }
        out.push(')');
    }
}

/// Everything the builders share.
#[derive(Clone)]
struct Ui {
    actions: Actions<Workbench>,
    door: LocalState<DoorState>,
    editor: LocalState<EditorInput>,
    selected: LocalState<Option<String>>,
    narrow: LocalState<NarrowScreen>,
    picker: LocalState<Option<String>>,
    sheet: LocalState<bool>,
    running: LocalState<Option<String>>,
}

impl Ui {
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        action: impl ui::tilo_resource::Binds<Workbench, I>,
    ) -> C {
        self.actions.bind(control, action)
    }

    /// `button`, invoking one door effect.
    fn door_button(&self, button: Button, effect: &DoorEffect) -> Button {
        self.actions.bind_effect_invocation(button, effect)
    }
}

/// The whole window.
pub(crate) fn root(cx: &ViewCx<Workbench>) -> Element {
    let ui = Ui {
        actions: cx.actions(),
        door: LocalState::new(DoorState::default()),
        editor: LocalState::new(EditorInput::default()),
        selected: LocalState::new(None),
        narrow: LocalState::new(NarrowScreen::List),
        picker: LocalState::new(None),
        sheet: LocalState::new(false),
        running: LocalState::new(None),
    };
    let core = Rc::new(RefCell::new(DoorCore::new(
        RemoteActions::current(),
        ui.door.clone(),
        cx.live().clone(),
    )));
    core.borrow_mut().refresh(None);
    crate::samples::stage(cx, &ui.door, &ui.selected, &ui.narrow, &ui.sheet);

    let follow = Rc::new(RefCell::new(follow(cx.live().clone(), &ui)));
    let shell = AppShell::new(library(cx, &ui), editor_pane(&ui, Rc::clone(&follow)))
        .with_id("shortcuts-shell")
        .with_sidebar_width(360.0)
        .with_breakpoint(820.0)
        .with_narrow_screen_state(&ui.narrow);
    let handler = Rc::clone(&core);
    cx.effects(move |effect: DoorEffect, cx| {
        let mut core = handler
            .try_borrow_mut()
            .map_err(|_| "the door is busy".to_owned())?;
        core.perform(effect, cx.reply())?;
        core.pump();
        Ok(Effected::Pending)
    });
    Box::new(
        Door::new(Box::new(shell), core).following(Box::new(move || {
            if let Ok(mut follow) = follow.try_borrow_mut() {
                follow();
            }
        })),
    )
}

/// What [`follow`] last derived the editor from: the open draft, the door,
/// the open picker and whether the save sheet shows.
type Followed = (Option<(Id, Draft)>, DoorState, Option<String>, bool);

/// Keep [`EditorInput`] in step with the draft, the door and the selection.
fn follow(live: Live<Workbench>, ui: &Ui) -> Box<dyn FnMut()> {
    let ui = ui.clone();
    let mut last_notice = None;
    let mut seen: Option<Followed> = None;
    Box::new(move || {
        let door = ui.door.get();
        if door.notice != last_notice {
            if matches!(door.notice, Some(Notice::Saved(_))) {
                ui.sheet.set(false);
            }
            last_notice.clone_from(&door.notice);
        }
        let selected = ui.selected.get();
        let draft = selected.and_then(|id| {
            let id = Id::from_raw(id);
            live.with(|workbench| workbench.drafts.get(&id).cloned())
                .map(|draft| (id, draft))
        });
        if draft.is_none() && ui.selected.with(Option::is_some) {
            ui.selected.set(None);
        }
        // An action just added from the picker closes it.
        let steps = |draft: &Option<(Id, Draft)>| {
            draft
                .as_ref()
                .map(|(id, draft)| (id.clone(), step_count(&draft.program.0.steps)))
        };
        if let (Some((before_id, before)), Some((now_id, now))) = (
            seen.as_ref().and_then(|(draft, ..)| steps(draft)),
            steps(&draft),
        ) && before_id == now_id
            && now > before
        {
            ui.picker.set(None);
        }
        let current = (draft, door, ui.picker.get(), ui.sheet.get());
        if seen.as_ref() == Some(&current) {
            return;
        }
        seen = Some(current.clone());
        let (draft, door, picker, sheet) = current;
        let catalog = door.catalog();
        let saved = door.library.ready().cloned().unwrap_or_default();
        let problems = draft
            .as_ref()
            .map(|(_, draft)| editor::problems(draft, &catalog, &saved))
            .unwrap_or_default();
        let next = EditorInput {
            draft,
            catalogue: match &door.catalogue {
                Loaded::Loading => Loaded::Loading,
                Loaded::Ready(_) => Loaded::Ready(()),
                Loaded::Failed(reason) => Loaded::Failed(reason.clone()),
            },
            catalog,
            saved,
            problems,
            picker,
            sheet,
        };
        if ui.editor.with(|current| *current != next) {
            ui.editor.set(next);
        }
    })
}

fn step_count(steps: &[Step]) -> usize {
    steps
        .iter()
        .map(|step| {
            1 + match &step.kind {
                StepKind::If {
                    then, otherwise, ..
                } => step_count(then) + step_count(otherwise),
                StepKind::ForEach { body, .. } => step_count(body),
                StepKind::Invoke { .. }
                | StepKind::SetVar { .. }
                | StepKind::Ask { .. }
                | StepKind::Ai { .. } => 0,
            }
        })
        .sum()
}

fn heading(key: &str) -> Text {
    Text::new(key)
        .with_role(TextRole::Heading)
        .with_heading_level(2)
}

fn caption(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Caption).dimmed()
}

// ---------------------------------------------------------------- library

fn library(cx: &ViewCx<Workbench>, ui: &Ui) -> Element {
    let new_draft = ui.bind(
        Button::new("new-shortcut", "shortcuts-new", None).primary(),
        Workbench::NEW_DRAFT.map(|()| String::from("New shortcut")),
    );
    let reload = ui.door_button(
        Button::new("reload", "shortcuts-reload", None),
        &DoorEffect::Refresh,
    );
    let notice = Text::from_state(&ui.door, |door| match &door.notice {
        Some(Notice::Saved(name)) => format!("Saved “{name}”"),
        Some(Notice::Deleted(name)) => format!("Deleted “{name}”"),
        Some(Notice::Failed(reason)) => format!("Could not finish: {reason}"),
        None => String::new(),
    })
    .with_id("door-notice")
    .with_role(TextRole::Caption)
    .with_live(ui::a11y::Politeness::Polite);

    let selected = ui.selected.clone();
    let narrow = ui.narrow.clone();
    let drafts = cx.each(
        |workbench: &Workbench| &workbench.drafts,
        move |row: &mut ViewCx<Workbench, Draft>| -> Element {
            let id = row
                .item()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_default();
            let name = row.text(|draft| {
                if draft.name.trim().is_empty() {
                    String::from("Untitled shortcut")
                } else {
                    draft.name.clone()
                }
            });
            let open = {
                let selected = selected.clone();
                let narrow = narrow.clone();
                Button::new(&format!("open-draft-{id}"), "shortcuts-edit", None).on_view(
                    move || {
                        selected.set(Some(id.clone()));
                        narrow.set(NarrowScreen::Content);
                    },
                )
            };
            Box::new(
                Row::new()
                    .push(Box::new(name))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(open)),
            )
        },
    );

    let saved = saved_list(ui);
    let runs = run_history(ui);
    let describe = describe_section(ui);
    let content = Column::new()
        .with_intrinsic_height()
        .with_gap(12.0)
        .push(Box::new(
            Text::new("shortcuts-title").with_role(TextRole::Title),
        ))
        .push(Box::new(ActionGroup::new(new_draft).secondary(reload)))
        .push(Box::new(notice))
        .push(describe)
        .push(Box::new(heading("shortcuts-drafts")))
        .push(Box::new(drafts))
        .push(Box::new(heading("shortcuts-saved")))
        .push(saved)
        .push(Box::new(heading("shortcuts-runs")))
        .push(runs);
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(content)),
    )
}

/// Asking the agent for a draft: shown only when the person is offered the
/// agent's transform to draft with. A drafted shortcut is only offered to
/// open as a draft; it is saved, like any other, after the grant review.
fn describe_section(ui: &Ui) -> Element {
    let builder = ui.clone();
    let description = LocalState::new(String::new());
    Box::new(ModelView::from_state_by(
        &ui.door,
        |door| {
            let offered = door
                .catalogue
                .ready()
                .is_some_and(|catalog| crate::drafting::target(catalog).is_some());
            (offered, door.drafted.clone())
        },
        move |door| -> Element {
            let offered = door
                .catalogue
                .ready()
                .is_some_and(|catalog| crate::drafting::target(catalog).is_some());
            if !offered {
                return Box::new(Spacer::fixed(0.0));
            }
            describe_panel(&builder, &description, door.drafted.as_ref())
        },
    ))
}

fn describe_panel(
    ui: &Ui,
    description: &LocalState<String>,
    drafted: Option<&door::Drafting>,
) -> Element {
    let field = TextInput::from_state(description)
        .with_id("describe-shortcut")
        .with_accessible_name("shortcuts-describe")
        .with_placeholder("shortcuts-describe-placeholder")
        .multiline(true);
    let source = description.clone();
    let ask = ui.actions.bind_effect(
        Button::new("draft-with-agent", "shortcuts-draft-it", None)
            .with_enabled(!matches!(drafted, Some(door::Drafting::Drafting))),
        DoorEffect::DESCRIBE.map(move |()| source.get()),
    );
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(8.0)
        .push(Box::new(heading("shortcuts-describe")))
        .push(Box::new(field))
        .push(Box::new(ActionGroup::new(ask)));
    match drafted {
        None => {}
        Some(door::Drafting::Drafting) => {
            column = column.push(Box::new(
                Row::new()
                    .push(Box::new(Spinner::new()))
                    .push(Box::new(caption("shortcuts-drafting"))),
            ));
        }
        Some(door::Drafting::Failed(reason)) => {
            column = column.push(Box::new(
                Banner::warning(ui::i18n!("shortcuts-draft-failed")).with_message(reason.clone()),
            ));
        }
        Some(door::Drafting::Ready(drafted)) => {
            let steps = drafted.shortcut.steps.len();
            column = column.push(Box::new(
                Text::new(&format!(
                    "“{}” · {steps} step{}",
                    drafted.shortcut.name,
                    if steps == 1 { "" } else { "s" }
                ))
                .with_id("drafted-summary")
                .with_role(TextRole::Label),
            ));
            if !drafted.problems.is_empty() {
                column = column.push(Box::new(
                    Banner::warning(ui::i18n!("shortcuts-draft-problems"))
                        .with_message(drafted.problems.join("\n")),
                ));
            }
            let json = serde_json::to_value(&drafted.shortcut).unwrap_or(Value::Null);
            let open = ui.bind(
                Button::new("open-drafted", "shortcuts-open-drafted", None).primary(),
                Workbench::OPEN_SHORTCUT.map(move |()| json.clone()),
            );
            column = column.push(Box::new(ActionGroup::new(open)));
        }
    }
    Box::new(column)
}

fn saved_list(ui: &Ui) -> Element {
    let ui = ui.clone();
    Box::new(ModelView::from_state_by(
        &ui.door.clone(),
        |door| {
            let library = match &door.library {
                Loaded::Loading => Loaded::Loading,
                Loaded::Failed(reason) => Loaded::Failed(reason.clone()),
                Loaded::Ready(library) => Loaded::Ready(
                    library
                        .iter()
                        .map(|shortcut| {
                            (
                                shortcut.id.clone(),
                                shortcut.name.clone(),
                                shortcut.inputs.len(),
                            )
                        })
                        .collect::<Vec<_>>(),
                ),
            };
            (library, door.catalog().rows().len())
        },
        move |door| -> Element {
            match &door.library {
                Loaded::Loading => Box::new(
                    Row::new()
                        .push(Box::new(Spinner::new()))
                        .push(Box::new(caption("shortcuts-loading"))),
                ),
                Loaded::Failed(reason) => Box::new(
                    Banner::warning(ui::i18n!("shortcuts-library-unavailable"))
                        .with_message(reason.clone()),
                ),
                Loaded::Ready(library) if library.is_empty() => Box::new(
                    EmptyState::new(ui::i18n!("shortcuts-saved-empty"))
                        .with_message(ui::i18n!("shortcuts-saved-empty-message")),
                ),
                Loaded::Ready(library) => {
                    let mut column = Column::new()
                        .with_intrinsic_height()
                        .with_no_padding()
                        .with_gap(8.0);
                    for shortcut in library {
                        column = column.push(saved_row(&ui, shortcut));
                    }
                    Box::new(column)
                }
            }
        },
    ))
}

fn saved_row(ui: &Ui, shortcut: &Shortcut) -> Element {
    let id = shortcut.id.clone();
    let last_run = {
        let key = id.clone();
        Text::from_state(&ui.door, move |door| {
            match door.last_run(&key).map(|run| &run.status) {
                None => String::from("Not run yet"),
                Some(RunStatus::Running) => String::from("Running…"),
                Some(RunStatus::Completed(_)) => String::from("Last run completed"),
                Some(RunStatus::Failed(reason)) => format!("Last run stopped: {reason}"),
            }
        })
        .with_id(format!("last-run-{id}"))
        .with_role(TextRole::Caption)
    };
    let run = if shortcut.inputs.is_empty() {
        ui.door_button(
            Button::new(&format!("run-{id}"), "shortcuts-run", None).primary(),
            &DoorEffect::Run {
                shortcut: id.clone(),
                inputs: None,
            },
        )
    } else {
        let running = ui.running.clone();
        let target = id.clone();
        Button::new(&format!("run-{id}"), "shortcuts-run-with", None)
            .primary()
            .on_view(move || running.set(Some(target.clone())))
    };
    let edit = ui.bind(
        Button::new(&format!("edit-{id}"), "shortcuts-edit", None),
        Workbench::OPEN_SHORTCUT.map({
            let json = serde_json::to_value(shortcut).unwrap_or(Value::Null);
            move |()| json.clone()
        }),
    );
    let delete = ui.door_button(
        Button::new(&format!("delete-{id}"), "shortcuts-delete", None).danger_outline(),
        &DoorEffect::Delete { shortcut: id },
    );
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(4.0)
        .push(Box::new(
            Text::new(&shortcut.name).with_role(TextRole::Label),
        ))
        .push(Box::new(last_run))
        .push(Box::new(
            ActionGroup::new(run).secondary(edit).secondary(delete),
        ));
    if !shortcut.inputs.is_empty() {
        column = column.push(run_inputs(ui, shortcut));
    }
    Box::new(column)
}

/// The inputs a run of `shortcut` starts with, shown once the person asks to
/// run it.
fn run_inputs(ui: &Ui, shortcut: &Shortcut) -> Element {
    let target = shortcut.id.clone();
    let builder = ui.clone();
    let shortcut = shortcut.clone();
    Box::new(ModelView::from_state_by(
        &ui.running,
        move |running| running.as_deref() == Some(target.as_str()),
        move |running| -> Element {
            if running.as_deref() == Some(shortcut.id.as_str()) {
                run_form(&builder, &shortcut)
            } else {
                Box::new(Spacer::fixed(0.0))
            }
        },
    ))
}

fn run_form(ui: &Ui, shortcut: &Shortcut) -> Element {
    let id = shortcut.id.clone();
    let fields: Vec<(ActionParam, LocalState<String>)> = shortcut
        .inputs
        .iter()
        .map(|input| (input.clone(), LocalState::new(String::new())))
        .collect();
    let mut form = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(6.0);
    for (input, value) in &fields {
        form = form.push(Box::new(labeled_field(
            input.description.to_string(),
            TextInput::from_state(value)
                .with_id(&format!("run-{id}-input-{}", input.name))
                .with_accessible_name(input.description.to_string()),
        )));
    }
    let start_id = format!("start-{id}");
    let start = ui.actions.bind_effect(
        Button::new(&start_id, "shortcuts-run", None).primary(),
        DoorEffect::RUN.map(move |()| {
            let inputs: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(input, value)| {
                    (input.name.to_string(), editor::literal(&value.get(), input))
                })
                .collect();
            (id.clone(), Some(inputs))
        }),
    );
    Box::new(form.push(Box::new(start)))
}

fn run_history(ui: &Ui) -> Element {
    Box::new(ModelView::from_state_by(
        &ui.door,
        |door| {
            door.runs
                .iter()
                .map(|run| (run.serial, run.name.clone(), run.status.clone()))
                .collect::<Vec<_>>()
        },
        |door| -> Element {
            if door.runs.is_empty() {
                return Box::new(caption("shortcuts-runs-empty"));
            }
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(4.0);
            for run in door.runs.iter().take(20) {
                let status = match &run.status {
                    RunStatus::Running => String::from("running"),
                    RunStatus::Completed(Some(value)) => format!("completed · {value}"),
                    RunStatus::Completed(None) => String::from("completed"),
                    RunStatus::Failed(reason) => format!("stopped · {reason}"),
                };
                column = column.push(Box::new(
                    Text::new(&format!("{} — {status}", run.name))
                        .with_id(format!("run-record-{}", run.serial)),
                ));
            }
            Box::new(column)
        },
    ))
}

// ----------------------------------------------------------------- editor

fn editor_pane(ui: &Ui, follow: Rc<RefCell<Box<dyn FnMut()>>>) -> Element {
    let builder = ui.clone();
    let editor = ui.editor.clone();
    let body = Keyed::new(
        move || {
            // Derive before keying, so a frame that only synchronizes (a
            // capture, the first frame) shows the current draft.
            if let Ok(mut follow) = follow.try_borrow_mut() {
                follow();
            }
            editor.with(EditorInput::structure)
        },
        move || {
            let content = builder.editor.with(|input| build_editor(&builder, input));
            // The column gives the scrolling part the pane's definite size
            // inside the stack that also holds the save sheet.
            Box::new(
                Column::new().with_no_padding().push(Box::new(
                    Scrollable::new_vertical()
                        .fill_parent_height()
                        .with_child(content),
                )),
            )
        },
    );
    let sheet_builder = ui.clone();
    let sheet_body = ModelView::from_state_by(
        &ui.editor,
        |input| {
            (
                input.draft.as_ref().map(|(id, draft)| {
                    (
                        id.clone(),
                        grant::least_privilege(&draft.program.0.steps, &input.catalog),
                    )
                }),
                input.problems.clone(),
            )
        },
        move |input| grant_body(&sheet_builder, input),
    );
    let sheet = ui.sheet.clone();
    let dialog = ModalDialog::new(ui::i18n!("shortcuts-grant-title"), sheet_body)
        .on_dismiss(move || sheet.set(false))
        .with_max_size(560.0, 520.0);
    Box::new(Stack::new().push(Box::new(body)).push(Box::new(Sheet {
        dialog,
        open: ui.sheet.clone(),
    })))
}

/// A part rebuilt whenever its key changes. Unlike `ModelView` it is not a
/// layout boundary of its own: the part is laid out in its place, so a
/// scrolling part can fill the pane it sits in.
struct Keyed<K> {
    key: Box<dyn FnMut() -> K>,
    build: Box<dyn FnMut() -> Element>,
    current: Option<K>,
    part: Element,
    dirty: bool,
}

impl<K: PartialEq + 'static> Keyed<K> {
    fn new(key: impl FnMut() -> K + 'static, build: impl FnMut() -> Element + 'static) -> Self {
        Self {
            key: Box::new(key),
            build: Box::new(build),
            current: None,
            part: Box::new(Spacer::fixed(0.0)),
            dirty: false,
        }
    }
}

impl<K: PartialEq + 'static> ui::widget::Decorator for Keyed<K> {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.part.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.part.as_mut()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let next = (self.key)();
        if self.current.as_ref() != Some(&next) {
            self.part = (self.build)();
            self.current = Some(next);
            self.dirty = true;
        }
        self.part.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty) | self.part.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "Keyed"
    }
}

/// What the save sheet's body is built from: the open draft with the
/// permissions it needs, and the checker's problems.
type SheetKey = (Option<(Id, Vec<Permission>)>, BTreeMap<String, Vec<String>>);

/// The save sheet: the shared dialog, shown while its state says so.
struct Sheet {
    dialog: ModalDialog<ModelView<SheetKey>>,
    open: LocalState<bool>,
}

impl ui::widget::Decorator for Sheet {
    type Inner = ModalDialog<ModelView<SheetKey>>;

    fn inner(&self) -> &ModalDialog<ModelView<SheetKey>> {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut ModalDialog<ModelView<SheetKey>> {
        &mut self.dialog
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let open = self.open.get();
        if open != self.dialog.is_open() {
            self.dialog.set_open(open);
        }
        self.dialog.sync(state, path);
    }

    fn name(&self) -> &'static str {
        "ShortcutSaveSheet"
    }
}

fn build_editor(ui: &Ui, input: &EditorInput) -> Element {
    let Some((id, draft)) = &input.draft else {
        return Box::new(
            EmptyState::new(ui::i18n!("shortcuts-editor-empty"))
                .with_message(ui::i18n!("shortcuts-editor-empty-message")),
        );
    };
    let draft_id = id.clone();
    let name = ui.bind(
        TextInput::local()
            .with_id("draft-name")
            .with_accessible_name("shortcuts-name")
            .with_initial_value(&draft.name),
        Workbench::RENAME.map({
            let draft = draft_id.clone();
            move |name: String| (draft.clone(), name)
        }),
    );
    let review = {
        let sheet = ui.sheet.clone();
        Button::new("review-save", "shortcuts-review", None)
            .primary()
            .on_view(move || sheet.set(true))
    };
    let discard = ui.bind(
        Button::new("discard-draft", "shortcuts-discard", None).danger_outline(),
        Workbench::DISCARD.map({
            let draft = draft_id.clone();
            move |()| draft.clone()
        }),
    );
    let problems = {
        let editor = ui.editor.clone();
        Text::from_state(&editor, |input| {
            let count: usize = input.problems.values().map(Vec::len).sum();
            match (input.problems.get(""), count) {
                (_, 0) => String::from("Ready to save"),
                (Some(top), _) => top.join("; "),
                (None, 1) => String::from("1 step needs attention"),
                (None, count) => format!("{count} problems to fix before saving"),
            }
        })
        .with_id("draft-problems")
        .with_role(TextRole::Caption)
        .with_live(ui::a11y::Politeness::Polite)
    };
    let mut content = Column::new()
        .with_intrinsic_height()
        .with_gap(16.0)
        .push(Box::new(labeled_field("shortcuts-name", name)))
        .push(Box::new(ActionGroup::new(review).secondary(discard)))
        .push(Box::new(problems));
    if let Loaded::Failed(reason) = &input.catalogue {
        content = content.push(Box::new(
            Banner::warning(ui::i18n!("shortcuts-catalogue-unavailable"))
                .with_message(reason.clone()),
        ));
    }
    content = content
        .push(inputs_section(ui, &draft_id, draft))
        .push(Box::new(heading("shortcuts-steps")))
        .push(block(ui, input, &draft_id, draft, "", 0))
        .push(output_section(ui, input, &draft_id, draft));
    Box::new(content)
}

fn inputs_section(ui: &Ui, draft_id: &Id, draft: &Draft) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(8.0)
        .push(Box::new(heading("shortcuts-inputs")));
    for input in &draft.program.0.inputs {
        let remove = ui.bind(
            Button::new(
                &format!("remove-input-{}", input.name),
                "shortcuts-remove",
                None,
            )
            .ghost(),
            Workbench::REMOVE_INPUT.map({
                let draft = draft_id.clone();
                let name = input.name.to_string();
                move |()| (draft.clone(), name.clone())
            }),
        );
        column = column.push(Box::new(
            Row::new()
                .push(Box::new(Text::new(&format!(
                    "{} · {}",
                    input.name,
                    editor::param_type_name(input.ty)
                ))))
                .push(Box::new(Spacer::new()))
                .push(Box::new(remove)),
        ));
    }
    let name = LocalState::new(String::new());
    let kind = LocalState::new(String::from("text"));
    let add = ui.bind(
        Button::new("add-input", "shortcuts-add-input", None),
        Workbench::ADD_INPUT.map({
            let draft = draft_id.clone();
            let name = name.clone();
            let kind = kind.clone();
            move |()| (draft.clone(), name.get().trim().to_owned(), kind.get())
        }),
    );
    column = column.push(Box::new(
        Row::new()
            .push(Box::new(
                TextInput::from_state(&name)
                    .with_id("new-input-name")
                    .with_accessible_name("shortcuts-input-name")
                    .with_placeholder("shortcuts-input-name"),
            ))
            .push(Box::new(
                Dropdown::from_state(
                    INPUT_TYPES.iter().map(|ty| (*ty).to_owned()).collect(),
                    &kind,
                )
                .with_id("new-input-type")
                .with_accessible_name("shortcuts-input-type"),
            ))
            .push(Box::new(add)),
    ));
    Box::new(column)
}

fn output_section(ui: &Ui, input: &EditorInput, draft_id: &Id, draft: &Draft) -> Element {
    let program = &draft.program.0;
    let values = values_at(
        &editor::candidate(draft, &input.catalog),
        &input.catalog,
        &editor::step_path(&program.steps.len().to_string()),
    );
    let choices: Vec<Choice> = values
        .into_iter()
        .map(|option| Choice {
            label: format!(
                "{} · {}",
                editor::describe(&option.binding),
                option.describes
            ),
            binding: option.binding,
        })
        .collect();
    let (values, labels, current) = choice_options(&choices, program.output.as_ref(), false);
    let state = LocalState::new(current);
    let mut picker = Dropdown::from_state(labels, &state)
        .with_id("draft-output")
        .with_accessible_name("shortcuts-output");
    picker.set_value_options(values);
    let picker = ui.bind(
        picker,
        Workbench::SET_OUTPUT.map({
            let draft = draft_id.clone();
            move |value: String| (draft.clone(), binding_text(&value))
        }),
    );
    Box::new(labeled_field("shortcuts-output", picker))
}

/// `(value, label)` pairs for a binding dropdown, the plain labels, and the
/// value of `current`.
fn choice_options(
    choices: &[Choice],
    current: Option<&Binding>,
    literal: bool,
) -> (Vec<(String, String)>, Vec<String>, String) {
    let mut values = vec![(UNSET.to_owned(), String::from("Not set"))];
    if literal {
        values.push((LITERAL.to_owned(), String::from("Type a value")));
    }
    for choice in choices {
        values.push((
            serde_json::to_string(&choice.binding).unwrap_or_default(),
            choice.label.clone(),
        ));
    }
    let selected = match current {
        None => UNSET.to_owned(),
        Some(Binding::Literal(_)) if literal => LITERAL.to_owned(),
        Some(binding) => serde_json::to_string(binding).unwrap_or_default(),
    };
    if !values.iter().any(|(value, _)| *value == selected)
        && let Some(binding) = current
    {
        values.push((selected.clone(), editor::describe(binding)));
    }
    let labels = values.iter().map(|(_, label)| label.clone()).collect();
    (values, labels, selected)
}

/// The binding a dropdown value stands for, as the JSON text the draft
/// actions take; empty for "not set".
fn binding_text(value: &str) -> String {
    binding_json(value)
        .map(|binding| binding.to_string())
        .unwrap_or_default()
}

/// The binding a dropdown value stands for; `None` for "not set".
fn binding_json(value: &str) -> Option<Value> {
    match value {
        UNSET => None,
        LITERAL => Some(json!({ "literal": "" })),
        other => serde_json::from_str(other).ok(),
    }
}

/// The steps of one block, then the controls that add to it.
fn block(
    ui: &Ui,
    input: &EditorInput,
    draft_id: &Id,
    draft: &Draft,
    address: &str,
    depth: usize,
) -> Element {
    let steps = block_at(&draft.program.0.steps, address).unwrap_or_default();
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(12.0);
    if depth > 0 {
        column = column.with_extra_left_padding(24.0);
    }
    if steps.is_empty() && depth == 0 {
        column = column.push(Box::new(caption("shortcuts-steps-empty")));
    }
    for (index, step) in steps.iter().enumerate() {
        let path = if address.is_empty() {
            index.to_string()
        } else {
            format!("{address}.{index}")
        };
        column = column.push(step_card(ui, input, draft_id, draft, step, &path, depth));
    }
    Box::new(column.push(add_controls(ui, input, draft_id, address)))
}

fn add_controls(ui: &Ui, input: &EditorInput, draft_id: &Id, address: &str) -> Element {
    let key = if address.is_empty() { "top" } else { address };
    if input.picker.as_deref() == Some(address) {
        return action_picker(ui, input, draft_id, address);
    }
    let open = {
        let picker = ui.picker.clone();
        let address = address.to_owned();
        Button::new(&format!("add-action-{key}"), "shortcuts-add-action", None)
            .on_view(move || picker.set(Some(address.clone())))
    };
    let block_button = |suffix: &str, label: &str, step: Value| {
        ui.bind(
            Button::new(&format!("add-{suffix}-{key}"), label, None).ghost(),
            Workbench::ADD_STEP.map({
                let draft = draft_id.clone();
                let address = address.to_owned();
                move |()| (draft.clone(), address.clone(), step.clone())
            }),
        )
    };
    Box::new(
        ActionGroup::new(open)
            .secondary(block_button(
                "if",
                "shortcuts-add-if",
                json!({"step": "if", "cond": {"truthy": {"literal": true}}, "then": []}),
            ))
            .secondary(block_button(
                "repeat",
                "shortcuts-add-repeat",
                json!({"step": "for_each", "items": {"literal": []}, "body": []}),
            ))
            .secondary(block_button(
                "set",
                "shortcuts-add-set",
                json!({"step": "set_var", "name": "value", "value": {"literal": ""}}),
            ))
            .secondary(block_button(
                "ask",
                "shortcuts-add-ask",
                json!({"step": "ask", "param": {
                    "name": "answer", "ty": "text", "required": true, "description": "Your answer"
                }}),
            ))
            .secondary(block_button(
                "ai",
                "shortcuts-add-ai",
                json!({"step": "ai", "transform": "summarise", "input": {"literal": ""}}),
            )),
    )
}

/// The live catalogue, grouped by app and resource kind, each action with
/// its typed parameters.
fn action_picker(ui: &Ui, input: &EditorInput, draft_id: &Id, address: &str) -> Element {
    let key = if address.is_empty() { "top" } else { address };
    let close = {
        let picker = ui.picker.clone();
        Button::new(&format!("close-picker-{key}"), "shortcuts-close", None)
            .ghost()
            .on_view(move || picker.set(None))
    };
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(8.0)
        .push(Box::new(
            Row::new()
                .push(Box::new(heading("shortcuts-add-action")))
                .push(Box::new(Spacer::new()))
                .push(Box::new(close)),
        ));
    let groups = editor::picker(&input.catalog);
    if groups.is_empty() {
        column = column.push(Box::new(caption(match input.catalogue {
            Loaded::Loading => "shortcuts-loading",
            _ => "shortcuts-catalogue-empty",
        })));
    }
    for group in groups {
        column = column.push(Box::new(
            Text::new(&group.label)
                .with_role(TextRole::Label)
                .with_heading_level(3),
        ));
        for kind in group.kinds {
            column = column.push(Box::new(caption(&kind.kind)));
            for offer in kind.offers {
                let step = serde_json::to_value(offer.step()).unwrap_or(Value::Null);
                let add = ui.bind(
                    Button::new(
                        &format!("offer-{}-{}-{}", offer.app_id, offer.resource, offer.action),
                        &format!("{} · {}", offer.resource_label, offer.label),
                        None,
                    ),
                    Workbench::ADD_STEP.map({
                        let draft = draft_id.clone();
                        let address = address.to_owned();
                        move |()| (draft.clone(), address.clone(), step.clone())
                    }),
                );
                let params = if offer.params.is_empty() {
                    String::from("no parameters")
                } else {
                    offer.params.join(", ")
                };
                column = column.push(Box::new(
                    Row::new()
                        .push(Box::new(add))
                        .push(Box::new(caption(&params))),
                ));
            }
        }
    }
    Box::new(column)
}

#[expect(
    clippy::too_many_lines,
    reason = "one card per step kind, read top to bottom like the editor"
)]
fn step_card(
    ui: &Ui,
    input: &EditorInput,
    draft_id: &Id,
    draft: &Draft,
    step: &Step,
    path: &str,
    depth: usize,
) -> Element {
    let at = StepEditor {
        ui,
        input,
        draft_id,
        draft,
        path,
    };
    let title = Text::new(&editor::title(step, &input.catalog)).with_role(TextRole::Label);
    let problems = {
        let key = path.to_owned();
        Text::from_state(&ui.editor, move |input| {
            input
                .problems
                .get(&key)
                .map(|reasons| reasons.join("; "))
                .unwrap_or_default()
        })
        .with_id(format!("step-problems-{path}"))
        .with_role(TextRole::Caption)
    };
    let up = ui.bind(
        Button::new(&format!("step-up-{path}"), "shortcuts-move-up", None).ghost(),
        Workbench::MOVE_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            move |()| (draft.clone(), path.clone(), true)
        }),
    );
    let down = ui.bind(
        Button::new(&format!("step-down-{path}"), "shortcuts-move-down", None).ghost(),
        Workbench::MOVE_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            move |()| (draft.clone(), path.clone(), false)
        }),
    );
    let remove = ui.bind(
        Button::new(&format!("step-remove-{path}"), "shortcuts-remove", None).ghost(),
        Workbench::REMOVE_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            move |()| (draft.clone(), path.clone())
        }),
    );
    let actions = ActionGroup::new(remove).secondary(up).secondary(down);
    let mut card = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(6.0)
        .push(Box::new(title))
        .push(Box::new(caption(&format!("Result: {}", step.id))))
        .push(Box::new(problems));
    card = match &step.kind {
        StepKind::Invoke { args, .. } => {
            let mut card = card;
            for param in editor::params_of(step, &input.catalog) {
                card = card.push(argument_row(
                    ui,
                    input,
                    draft_id,
                    draft,
                    path,
                    &param,
                    args.get(param.name.as_ref()),
                ));
            }
            card
        }
        StepKind::If {
            cond,
            then,
            otherwise,
        } => {
            let current = match cond {
                Condition::Truthy(binding) => Some(binding),
                _ => None,
            };
            let row = value_row(&at, "condition", "shortcuts-condition", current, {
                let then = then.clone();
                let otherwise = otherwise.clone();
                move |binding| json!({"step": "if", "cond": {"truthy": binding}, "then": then, "else": otherwise})
            });
            card.push(row)
                .push(Box::new(caption("shortcuts-then")))
                .push(block(
                    ui,
                    input,
                    draft_id,
                    draft,
                    &format!("{path}.then"),
                    depth + 1,
                ))
                .push(Box::new(caption("shortcuts-otherwise")))
                .push(block(
                    ui,
                    input,
                    draft_id,
                    draft,
                    &format!("{path}.else"),
                    depth + 1,
                ))
        }
        StepKind::ForEach { items, item, body } => {
            let row = value_row(&at, "items", "shortcuts-items", Some(items), {
                let item = item.clone();
                let body = body.clone();
                move |binding| json!({"step": "for_each", "items": binding, "item": item, "body": body})
            });
            card.push(row)
                .push(Box::new(caption(&format!(
                    "Each item is the variable {item}"
                ))))
                .push(block(
                    ui,
                    input,
                    draft_id,
                    draft,
                    &format!("{path}.each"),
                    depth + 1,
                ))
        }
        StepKind::SetVar { name, value } => {
            let name_field = step_text(&at, "variable-name", "shortcuts-variable", name, {
                let value = value.clone();
                move |text| json!({"step": "set_var", "name": text, "value": value})
            });
            let row = value_row(&at, "value", "shortcuts-value", Some(value), {
                let name = name.clone();
                move |binding| json!({"step": "set_var", "name": name, "value": binding})
            });
            card.push(name_field).push(row)
        }
        StepKind::Ask { param } => {
            let question = step_text(&at, "question", "shortcuts-question", &param.description, {
                let param = param.clone();
                move |text| {
                    let mut param = param.clone();
                    param.description = text.into();
                    json!({"step": "ask", "param": param})
                }
            });
            card.push(question)
        }
        StepKind::Ai {
            transform,
            input: source,
        } => {
            let mut card = card;
            let row = value_row(&at, "ai-input", "shortcuts-ai-input", Some(source), {
                let transform = transform.clone();
                move |binding| json!({"step": "ai", "transform": transform, "input": binding})
            });
            card = card.push(row);
            if let AiTransform::Freeform { prompt } = transform {
                card = card.push(step_text(&at, "ai-prompt", "shortcuts-ai-prompt", prompt, {
                    let source = source.clone();
                    move |text| json!({"step": "ai", "transform": {"freeform": {"prompt": text}}, "input": source})
                }));
            }
            card
        }
    };
    Box::new(card.push(Box::new(actions)))
}

/// The step at `path` of the draft being edited, and what its controls read.
struct StepEditor<'a> {
    ui: &'a Ui,
    input: &'a EditorInput,
    draft_id: &'a Id,
    draft: &'a Draft,
    path: &'a str,
}

/// A text field that rewrites what the step does from its value.
fn step_text(
    at: &StepEditor<'_>,
    suffix: &str,
    label: &str,
    current: &str,
    rewrite: impl Fn(String) -> Value + Send + Sync + 'static,
) -> Element {
    let StepEditor {
        ui, draft_id, path, ..
    } = *at;
    let field = ui.bind(
        TextInput::local()
            .with_id(&format!("step-{suffix}-{path}"))
            .with_accessible_name(label)
            .with_initial_value(current),
        Workbench::SET_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            move |text: String| (draft.clone(), path.clone(), rewrite(text))
        }),
    );
    Box::new(labeled_field(label, field))
}

/// A dropdown choosing where a block value (a condition, the items of a
/// repeat, a variable's value, an AI step's input) comes from, rewriting the
/// step with `rewrite`.
fn value_row(
    at: &StepEditor<'_>,
    suffix: &str,
    label: &str,
    current: Option<&Binding>,
    rewrite: impl Fn(Value) -> Value + Send + Sync + 'static,
) -> Element {
    let StepEditor {
        ui,
        input,
        draft_id,
        draft,
        path,
    } = *at;
    let shortcut = editor::candidate(draft, &input.catalog);
    let choices: Vec<Choice> = values_at(&shortcut, &input.catalog, &editor::step_path(path))
        .into_iter()
        .map(|option| Choice {
            label: format!(
                "{} · {}",
                editor::describe(&option.binding),
                option.describes
            ),
            binding: option.binding,
        })
        .collect();
    let (values, labels, selected) = choice_options(&choices, current, true);
    let state = LocalState::new(selected.clone());
    let mut picker = Dropdown::from_state(labels, &state)
        .with_id(&format!("step-{suffix}-source-{path}"))
        .with_accessible_name(label);
    picker.set_value_options(values);
    let rewrite = std::sync::Arc::new(rewrite);
    let picker = ui.bind(
        picker,
        Workbench::SET_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            let rewrite = std::sync::Arc::clone(&rewrite);
            move |value: String| {
                let binding = binding_json(&value).unwrap_or_else(|| json!({"literal": null}));
                (draft.clone(), path.clone(), rewrite(binding))
            }
        }),
    );
    let mut row = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(4.0)
        .push(Box::new(labeled_field(label, picker)));
    if selected == LITERAL
        && let Some(Binding::Literal(value)) = current
    {
        let text = editor::literal_text(value);
        let param =
            ActionParam::required_owned(suffix.to_owned(), ParamType::Text, label.to_owned());
        let rewrite_literal = rewrite;
        let field = ui.bind(
            TextInput::local()
                .with_id(&format!("step-{suffix}-literal-{path}"))
                .with_accessible_name(label)
                .with_initial_value(&text),
            Workbench::SET_STEP.map({
                let draft = draft_id.clone();
                let path = path.to_owned();
                move |text: String| {
                    let literal = editor::literal(&text, &param);
                    (
                        draft.clone(),
                        path.clone(),
                        rewrite_literal(json!({"literal": literal})),
                    )
                }
            }),
        );
        row = row.push(Box::new(field));
    }
    Box::new(row)
}

/// One parameter of an action step: where its value comes from and, for a
/// typed value, the control that edits it.
fn argument_row(
    ui: &Ui,
    input: &EditorInput,
    draft_id: &Id,
    draft: &Draft,
    path: &str,
    param: &ActionParam,
    current: Option<&Binding>,
) -> Element {
    let name = param.name.to_string();
    let label = format!(
        "{}{} · {}",
        param.name,
        if param.required { "" } else { " (optional)" },
        editor::param_type_name(param.ty)
    );
    let choices = editor::choices(draft, &input.catalog, path, param);
    let (values, labels, selected) = choice_options(&choices, current, true);
    let state = LocalState::new(selected);
    let mut source = Dropdown::from_state(labels, &state)
        .with_id(&format!("arg-{path}-{name}"))
        .with_accessible_name(label.clone());
    source.set_value_options(values);
    let source = ui.bind(
        source,
        Workbench::SET_ARGUMENT.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            let param = param.clone();
            move |value: String| {
                let binding = if value == LITERAL {
                    json!({ "literal": default_literal(&param) }).to_string()
                } else {
                    binding_text(&value)
                };
                (draft.clone(), path.clone(), name.clone(), binding)
            }
        }),
    );
    let mut row = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(4.0)
        .push(Box::new(labeled_field(&label, source)));
    if let Some(Binding::Literal(value)) = current {
        row = row.push(literal_control(ui, draft_id, path, param, value));
    }
    Box::new(row)
}

fn default_literal(param: &ActionParam) -> Value {
    match param.ty {
        ParamType::Boolean => Value::Bool(false),
        ParamType::Integer | ParamType::Number => json!(0),
        ParamType::Object => json!({}),
        ParamType::Array => json!([]),
        ParamType::Text | ParamType::DateTime | ParamType::Secret | ParamType::Resource => {
            Value::String(String::new())
        }
    }
}

/// The typed control a literal is edited with.
fn literal_control(
    ui: &Ui,
    draft_id: &Id,
    path: &str,
    param: &ActionParam,
    value: &Value,
) -> Element {
    let id = format!("literal-{path}-{}", param.name);
    let draft = draft_id.clone();
    let path = path.to_owned();
    let name = param.name.to_string();
    match param.ty {
        ParamType::Boolean => {
            let state = LocalState::new(value.as_bool().unwrap_or(false));
            Box::new(ui.bind(
                Toggle::from_state(&state).with_id(&id),
                Workbench::SET_ARGUMENT.map(move |on: bool| {
                    (
                        draft.clone(),
                        path.clone(),
                        name.clone(),
                        json!({ "literal": on }).to_string(),
                    )
                }),
            ))
        }
        ParamType::Integer | ParamType::Number => {
            let integer = param.ty == ParamType::Integer;
            let state = LocalState::new(value.as_f64().unwrap_or(0.0));
            Box::new(
                ui.bind(
                    NumberInput::from_state(&state)
                        .with_id(&id)
                        .with_accessible_name(param.name.to_string())
                        .with_precision(if integer { 0 } else { 2 }),
                    Workbench::SET_ARGUMENT.map(move |number: f64| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "an integer parameter's control steps in whole numbers"
                        )]
                        let literal = if integer {
                            json!(number.round() as i64)
                        } else {
                            json!(number)
                        };
                        (
                            draft.clone(),
                            path.clone(),
                            name.clone(),
                            json!({ "literal": literal }).to_string(),
                        )
                    }),
                ),
            )
        }
        _ => {
            let param = param.clone();
            Box::new(
                ui.bind(
                    TextInput::local()
                        .with_id(&id)
                        .with_accessible_name(param.name.to_string())
                        .with_initial_value(&editor::literal_text(value)),
                    Workbench::SET_ARGUMENT.map(move |text: String| {
                        let literal = editor::literal(&text, &param);
                        (
                            draft.clone(),
                            path.clone(),
                            name.clone(),
                            json!({ "literal": literal }).to_string(),
                        )
                    }),
                ),
            )
        }
    }
}

/// What the shortcut will be allowed to do, shown before it is saved.
/// What the shortcut will be allowed to do, shown before it is saved.
fn grant_body(ui: &Ui, input: &EditorInput) -> Element {
    let Some((draft_id, draft)) = &input.draft else {
        return Box::new(Spacer::fixed(0.0));
    };
    let permissions: Vec<Permission> =
        grant::least_privilege(&draft.program.0.steps, &input.catalog);
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(8.0)
        .push(Box::new(Text::new("shortcuts-grant-explain")));
    if permissions.is_empty() {
        body = body.push(Box::new(caption("shortcuts-grant-none")));
    }
    for permission in &permissions {
        body = body.push(Box::new(
            Text::new(&format!(
                "{} — {}",
                permission.label,
                verb_label(permission.verb)
            ))
            .with_id(format!("grant-{}-{}", permission.app_id, permission.action)),
        ));
    }
    let blocked = !input.problems.is_empty();
    if blocked {
        body = body.push(Box::new(
            Banner::warning(ui::i18n!("shortcuts-grant-blocked")).with_message(
                input
                    .problems
                    .iter()
                    .map(|(path, reasons)| {
                        format!(
                            "{}: {}",
                            if path.is_empty() { "shortcut" } else { path },
                            reasons.join("; ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ));
    }
    let cancel = {
        let sheet = ui.sheet.clone();
        Button::new("grant-cancel", "shortcuts-cancel", None).on_view(move || sheet.set(false))
    };
    let save = ui
        .door_button(
            Button::new("grant-save", "shortcuts-save", None).primary(),
            &DoorEffect::Save {
                draft: draft_id.as_str().to_owned(),
            },
        )
        .with_enabled(!blocked);
    Box::new(body.push(Box::new(ActionGroup::new(save).secondary(cancel))))
}

const fn verb_label(verb: collab_types::Verb) -> &'static str {
    match verb {
        collab_types::Verb::Read => "read",
        collab_types::Verb::Comment => "comment",
        collab_types::Verb::Write => "change",
        collab_types::Verb::Invite => "invite",
        collab_types::Verb::Admin => "administer",
    }
}
