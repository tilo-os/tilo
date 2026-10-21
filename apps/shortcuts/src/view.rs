//! The window: the library beside the page of whatever is open in it.
//!
//! The sidebar lists the overview, the drafts and the saved shortcuts; the
//! content is the page of the selected one: the overview (asking the agent
//! for a draft, recent runs), the editor of a draft, or a saved shortcut with
//! its run, edit and delete actions.
//!
//! Every control is bound to a published action — a document action on the
//! drafts, a door effect, or a view action for what is only presentation
//! (which page is open, whether a sheet shows). The editor is rebuilt when
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
use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::Path;
use ui::widget::{
    Actions as ActionGroup, ButtonRole, ControlSize, ListRow, ModalDialog, NumberInput, Section,
    SectionHeader, labeled_field,
};
use ui::{LocalState, RemoteActions, TextKind};

use crate::door::{self, Door, DoorCore, DoorEffect, DoorState, Loaded, Notice, RunStatus};
use crate::editor::{self, Choice};
use crate::model::{Draft, INPUT_TYPES, Workbench, block_at};
use tilo_automation::grant::{self, Permission};

/// The size of the controls that sit with text fields in a form. A text
/// field is as tall as a `Large` control, so dropdowns and buttons beside or
/// above one are too, and the edges line up.
const FIELD_HEIGHT: ControlSize = ControlSize::Large;
/// Choice value meaning "type a value here".
const LITERAL: &str = "literal";
/// Choice value meaning "not bound".
const UNSET: &str = "unset";

/// The sidebar item of the overview page.
pub(crate) const OVERVIEW: &str = "overview";
/// What a draft's sidebar item id starts with; the draft's id follows.
pub(crate) const DRAFT: &str = "draft:";
/// What a saved shortcut's sidebar item id starts with; the shortcut's id
/// follows.
pub(crate) const SAVED: &str = "saved:";

/// What the editor is built from: the open page and what the door knows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorInput {
    pub draft: Option<(Id, Draft)>,
    /// The saved shortcut whose page is open, when no draft is.
    pub open_saved: Option<String>,
    pub catalog: ActionCatalog,
    pub catalogue: Loaded<()>,
    pub saved: Vec<Shortcut>,
    pub problems: BTreeMap<String, Vec<String>>,
    pub picker: Option<String>,
    pub sheet: bool,
}

impl EditorInput {
    /// What the built page depends on: everything but typed values.
    fn structure(&self) -> String {
        let Some((id, draft)) = &self.draft else {
            let open = self
                .open_saved
                .as_ref()
                .and_then(|id| self.saved.iter().find(|saved| &saved.id == id))
                .map(|saved| {
                    (
                        &saved.id,
                        &saved.name,
                        saved.steps.len(),
                        saved.inputs.len(),
                    )
                });
            return format!(
                "none|{:?}|{}|{open:?}",
                self.catalogue,
                self.catalog.rows().len()
            );
        };
        let program = &draft.program();
        let mut shape = String::new();
        shape_of(&program.steps, &mut shape);
        // What each step reports is part of how its card is built; what the
        // shortcut itself reports (its name) changes as the name is typed.
        let step_problems: Vec<_> = self
            .problems
            .iter()
            .filter(|(path, _)| !path.is_empty())
            .collect();
        format!(
            "{}|{}|{:?}|{}|{}|{:?}|{:?}|{}|{}|{step_problems:?}",
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
                action,
                resource,
                args,
                ..
            } => {
                let _ = write!(out, "{action}/{resource:?}");
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
    live: Live<Workbench>,
    core: Rc<RefCell<DoorCore>>,
    door: LocalState<DoorState>,
    editor: LocalState<EditorInput>,
    /// The sidebar item of the open page.
    selected: LocalState<Option<String>>,
    /// The front screen of a narrow window.
    pane: LocalState<NavPane>,
    picker: LocalState<Option<String>>,
    sheet: LocalState<bool>,
}

impl Ui {
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        action: impl ui::tilo_resource::Binds<Workbench, I>,
    ) -> C {
        self.actions.bind(control, action)
    }

    /// `control`, setting the field at `path` to what `value` makes of its
    /// input.
    fn edit<T, I, C>(
        &self,
        control: C,
        path: Path<Workbench, T>,
        value: impl Fn(I) -> T + Send + Sync + 'static,
    ) -> C
    where
        T: ui::tilo_resource::Settable + 'static,
        I: 'static,
        C: ActionControl<I>,
    {
        let binding = self
            .actions
            .edit_binding(path.cast::<I>(), move |path, input| {
                path.clone().cast::<T>().set(value(input))
            });
        match binding {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `button`, invoking one door effect.
    fn door_button(&self, button: Button, effect: &DoorEffect) -> Button {
        self.actions.bind_effect_invocation(button, effect)
    }

    /// Show the page of sidebar item `item`, also in a narrow window.
    fn open(&self, item: String) {
        self.selected.set(Some(item));
        self.pane.set(NavPane::Content);
    }
}

/// The whole window.
pub(crate) fn root(cx: &ViewCx<Workbench>) -> Element {
    let door_state = LocalState::new(DoorState::default());
    let core = Rc::new(RefCell::new(DoorCore::new(
        RemoteActions::current(),
        door_state.clone(),
        cx.live().clone(),
    )));
    let ui = Ui {
        actions: cx.actions(),
        live: cx.live().clone(),
        core: Rc::clone(&core),
        door: door_state,
        editor: LocalState::new(EditorInput::default()),
        selected: LocalState::new(Some(OVERVIEW.to_owned())),
        pane: LocalState::new(NavPane::Sidebar),
        picker: LocalState::new(None),
        sheet: LocalState::new(false),
    };
    // A past version or a capture shows the door as staged, never the
    // session's.
    if cx.mode().is_live() {
        core.borrow_mut().refresh(None);
    }
    crate::samples::stage(cx, &ui.door, &ui.selected, &ui.pane, &ui.sheet, &ui.picker);

    let follow = Rc::new(RefCell::new(follow(cx.live().clone(), &ui)));
    let shell = NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("shortcuts-shell")
        .mode(NavMode::Push)
        .with_pane_state(&ui.pane)
        .sidebar(Box::new(LibraryNav::new(&ui)))
        .sidebar_header("", sidebar_actions(&ui))
        .content(editor_pane(&ui, Rc::clone(&follow)));
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

/// What [`follow`] last derived the editor from: the open sidebar item, the
/// open draft, the door, the open picker and whether the save sheet shows.
type Followed = (
    Option<String>,
    Option<(Id, Draft)>,
    DoorState,
    Option<String>,
    bool,
);

/// Keep [`EditorInput`] in step with the draft, the door and the selection.
fn follow(live: Live<Workbench>, ui: &Ui) -> Box<dyn FnMut()> {
    let ui = ui.clone();
    let mut last_notice = None;
    let mut seen: Option<Followed> = None;
    let mut known: Option<Vec<String>> = None;
    Box::new(move || {
        let door = ui.door.get();
        if door.notice != last_notice {
            if matches!(door.notice, Some(Notice::Saved(_))) {
                ui.sheet.set(false);
            }
            last_notice.clone_from(&door.notice);
        }
        // A draft that appears (a new one, a saved shortcut opened to edit,
        // one the agent drafted) is the page to show.
        let ids: Vec<String> = live.with(|workbench| {
            workbench
                .drafts
                .ids()
                .map(|id| id.as_str().to_owned())
                .collect()
        });
        if let Some(before) = &known
            && let Some(added) = ids.iter().find(|id| !before.contains(id))
        {
            ui.open(format!("{DRAFT}{added}"));
        }
        known = Some(ids);

        let mut selected = ui.selected.get();
        let mut draft = selected
            .as_deref()
            .and_then(|item| item.strip_prefix(DRAFT))
            .and_then(|id| {
                let id = Id::from_raw(id);
                live.with(|workbench| workbench.drafts.get(&id).cloned())
                    .map(|draft| (id, draft))
            });
        let mut open_saved = selected
            .as_deref()
            .and_then(|item| item.strip_prefix(SAVED))
            .map(str::to_owned);
        // What was open is gone (discarded, deleted): back to the overview.
        let gone = match selected.as_deref() {
            Some(item) if item.starts_with(DRAFT) => draft.is_none(),
            Some(item) if item.starts_with(SAVED) => door.library.ready().is_some_and(|library| {
                !library
                    .iter()
                    .any(|shortcut| Some(&shortcut.id) == open_saved.as_ref())
            }),
            _ => false,
        };
        if gone {
            ui.selected.set(Some(OVERVIEW.to_owned()));
            selected = Some(OVERVIEW.to_owned());
            draft = None;
            open_saved = None;
        }
        // An action just added from the picker closes it.
        let steps = |draft: &Option<(Id, Draft)>| {
            draft
                .as_ref()
                .map(|(id, draft)| (id.clone(), step_count(&draft.program().steps)))
        };
        if let (Some((before_id, before)), Some((now_id, now))) = (
            seen.as_ref().and_then(|(_, draft, ..)| steps(draft)),
            steps(&draft),
        ) && before_id == now_id
            && now > before
        {
            ui.picker.set(None);
        }
        let current = (selected, draft, door, ui.picker.get(), ui.sheet.get());
        if seen.as_ref() == Some(&current) {
            return;
        }
        seen = Some(current.clone());
        let (_, draft, door, picker, sheet) = current;
        let catalog = door.catalog();
        let saved = door.library.ready().cloned().unwrap_or_default();
        let problems = draft
            .as_ref()
            .map(|(_, draft)| editor::problems(draft, &catalog, &saved))
            .unwrap_or_default();
        let next = EditorInput {
            draft,
            open_saved,
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

fn caption(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Caption).dimmed()
}

/// A name shown for a draft that has none yet.
fn draft_label(name: &str) -> String {
    if name.trim().is_empty() {
        String::from("Untitled shortcut")
    } else {
        name.to_owned()
    }
}

// ---------------------------------------------------------------- sidebar

/// The sidebar's actions: start a shortcut, read the library again.
fn sidebar_actions(ui: &Ui) -> Vec<Element> {
    let new_draft = ui.bind(
        Button::icon_only("new-shortcut", Icon::Plus, "New shortcut"),
        Workbench::NEW_DRAFT.map(|()| String::from("New shortcut")),
    );
    let reload = ui.door_button(
        Button::icon_only("reload", Icon::Refresh, "Reload shortcuts"),
        &DoorEffect::Refresh,
    );
    vec![Box::new(new_draft), Box::new(reload)]
}

/// The destinations: the overview, then the drafts and the saved shortcuts.
fn library_sections(live: &Live<Workbench>, door: &LocalState<DoorState>) -> Vec<NavSection> {
    let drafts: Vec<NavItem> = live.with(|workbench| {
        workbench
            .drafts
            .iter()
            .map(|(id, draft)| {
                NavItem::new(format!("{DRAFT}{}", id.as_str()), draft_label(&draft.name))
                    .icon(Icon::Pencil)
            })
            .collect()
    });
    // `None` until the library has loaded: then the section says nothing
    // rather than that there is nothing.
    let saved: Option<Vec<NavItem>> = door.with(|door| {
        door.library.ready().map(|library| {
            library
                .iter()
                .map(|shortcut| {
                    NavItem::new(format!("{SAVED}{}", shortcut.id), shortcut.name.clone())
                        .icon(Icon::Play)
                })
                .collect()
        })
    });
    let mut sections = vec![NavSection::untitled(
        "overview",
        vec![NavItem::new(OVERVIEW, "Overview").icon(Icon::LayoutGrid)],
    )];
    sections.push(NavSection::new("drafts", "Drafts", drafts).empty_caption("No drafts"));
    if let Some(saved) = saved {
        sections.push(NavSection::new("saved", "Saved", saved).empty_caption("No saved shortcuts"));
    }
    sections
}

/// The navigation list, kept in step with the drafts and the library.
struct LibraryNav {
    nav: NavList,
    live: Live<Workbench>,
    door: LocalState<DoorState>,
}

impl LibraryNav {
    fn new(ui: &Ui) -> Self {
        let nav = NavList::new(library_sections(&ui.live, &ui.door))
            .bind_selection(&ui.selected)
            .with_id("shortcuts-library")
            .with_accessible_name("Shortcuts");
        Self {
            nav,
            live: ui.live.clone(),
            door: ui.door.clone(),
        }
    }
}

impl ui::widget::Decorator for LibraryNav {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.nav
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.nav
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.nav
            .set_sections(library_sections(&self.live, &self.door));
        Widget::sync(&mut self.nav, state, path);
    }
}

// ------------------------------------------------------------------ pages

/// A page: a column of what it shows, under the outcome of the last save or
/// delete.
fn page(ui: &Ui, items: Vec<Element>) -> Element {
    let mut page = Column::new().with_intrinsic_height().push(notice(ui));
    for item in items {
        page = page.push(item);
    }
    // Forms and prose stay at the readable width on the leading edge,
    // however wide the window is.
    Box::new(
        Container::new()
            .with_no_padding()
            .readable()
            .with_child(Box::new(page)),
    )
}

/// What the last save or delete came to, until the next one.
fn notice(ui: &Ui) -> Element {
    Box::new(ModelView::from_state_by(
        &ui.door,
        |door| door.notice.clone(),
        |door| -> Element {
            match &door.notice {
                Some(Notice::Saved(name)) => {
                    Box::new(Banner::success(format!("Saved “{name}”")).dismissible())
                }
                Some(Notice::Deleted(name)) => {
                    Box::new(Banner::info(format!("Deleted “{name}”")).dismissible())
                }
                Some(Notice::Failed(reason)) => Box::new(
                    Banner::error("Could not finish")
                        .with_message(reason.clone())
                        .dismissible(),
                ),
                None => ui::widget::optional(None),
            }
        },
    ))
}

// --------------------------------------------------------------- overview

/// Whether the workspace offers the agent's transform to draft with.
fn drafting_offered(door: &DoorState) -> bool {
    door.catalogue
        .ready()
        .is_some_and(|catalog| crate::drafting::target(catalog).is_some())
}

fn overview_page(ui: &Ui) -> Element {
    let builder = ui.clone();
    let description = LocalState::new(String::new());
    let body = ModelView::from_state_by(
        &ui.door,
        |door| {
            (
                drafting_offered(door),
                door.drafted.clone(),
                match &door.library {
                    Loaded::Loading => Loaded::Loading,
                    Loaded::Failed(reason) => Loaded::Failed(reason.clone()),
                    Loaded::Ready(library) => Loaded::Ready(library.len()),
                },
                door.runs
                    .iter()
                    .map(|run| (run.serial, run.name.clone(), run.status.clone()))
                    .collect::<Vec<_>>(),
            )
        },
        move |door| -> Element { overview_body(&builder, &description, door) },
    );
    page(ui, vec![Box::new(body)])
}

fn overview_body(ui: &Ui, description: &LocalState<String>, door: &DoorState) -> Element {
    let mut items: Vec<Element> = Vec::new();
    let drafting = drafting_offered(door);
    if drafting {
        items.push(describe_panel(ui, description, door.drafted.as_ref()));
    }
    match &door.library {
        Loaded::Loading => items.push(Box::new(
            Row::new()
                .push(Box::new(Spinner::new()))
                .push(Box::new(caption("shortcuts-loading"))),
        )),
        Loaded::Failed(reason) => items.push(Box::new(
            Banner::warning(ui::i18n!("shortcuts-library-unavailable"))
                .with_message(reason.clone()),
        )),
        Loaded::Ready(_) => {}
    }
    if door.runs.is_empty() {
        if matches!(&door.library, Loaded::Ready(library) if library.is_empty()) {
            items.push(Box::new(
                EmptyState::new(ui::i18n!("shortcuts-saved-empty")).with_message(ui::i18n!(
                    if drafting {
                        "shortcuts-saved-empty-draft"
                    } else {
                        "shortcuts-saved-empty-blank"
                    }
                )),
            ));
        } else {
            items.push(Box::new(Section::titled(
                ui::i18n!("shortcuts-runs"),
                vec![Box::new(caption("shortcuts-runs-empty"))],
            )));
        }
    } else {
        items.push(run_history(ui, door));
    }
    Box::new(Section::new(items))
}

/// Asking the agent for a draft: shown only when the person is offered the
/// agent's transform to draft with. A drafted shortcut is only offered to
/// open as a draft; it is saved, like any other, after the grant review.
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
    let mut items: Vec<Element> = vec![Box::new(field)];
    match drafted {
        None => {}
        Some(door::Drafting::Drafting) => {
            items.push(Box::new(
                Row::new()
                    .push(Box::new(Spinner::new()))
                    .push(Box::new(caption("shortcuts-drafting"))),
            ));
        }
        Some(door::Drafting::Failed(reason)) => {
            items.push(Box::new(
                Banner::warning(ui::i18n!("shortcuts-draft-failed")).with_message(reason.clone()),
            ));
        }
        Some(door::Drafting::Ready(drafted)) => {
            let steps = drafted.shortcut.steps.len();
            items.push(Box::new(
                Text::new(&format!(
                    "“{}” has {steps} step{}",
                    drafted.shortcut.name,
                    if steps == 1 { "" } else { "s" }
                ))
                .with_id("drafted-summary")
                .with_role(TextRole::Label),
            ));
            if !drafted.problems.is_empty() {
                items.push(Box::new(
                    Banner::warning(ui::i18n!("shortcuts-draft-problems"))
                        .with_message(drafted.problems.join("\n")),
                ));
            }
        }
    }
    // The buttons follow what is typed, so asking is offered only once there
    // is something to ask; the field above is not rebuilt with them.
    let buttons = {
        let ui = ui.clone();
        let typed = description.clone();
        ModelView::from_document(
            {
                let door = ui.door.clone();
                let typed = typed.clone();
                move |_, _| {
                    (
                        door.with(|door| door.drafted.clone()),
                        typed.get().trim().is_empty(),
                    )
                }
            },
            move |(drafted, empty)| describe_buttons(&ui, &typed, drafted.as_ref(), *empty),
        )
    };
    items.push(Box::new(buttons));
    Box::new(Section::titled(ui::i18n!("shortcuts-describe"), items))
}

/// Draft it, and once there is a draft, open it.
fn describe_buttons(
    ui: &Ui,
    description: &LocalState<String>,
    drafted: Option<&door::Drafting>,
    empty: bool,
) -> Element {
    let source = description.clone();
    // Asking is the main action until there is a draft to open.
    let role = if matches!(drafted, Some(door::Drafting::Ready(_))) {
        ButtonRole::Secondary
    } else {
        ButtonRole::Primary
    };
    let ask = ui.actions.bind_effect(
        Button::new("draft-with-agent", "shortcuts-draft-it")
            .role(role)
            .with_enabled(!empty && !matches!(drafted, Some(door::Drafting::Drafting))),
        DoorEffect::DESCRIBE.map(move |()| source.get()),
    );
    // Opening a draft is the main action once there is one; asking again is
    // the quieter choice beside it.
    Box::new(match drafted {
        Some(door::Drafting::Ready(drafted)) => {
            let json = serde_json::to_value(&drafted.shortcut).unwrap_or(Value::Null);
            let open = ui.bind(
                Button::new("open-drafted", "shortcuts-open-drafted").role(ButtonRole::Primary),
                Workbench::OPEN_SHORTCUT.map(move |()| json.clone()),
            );
            ActionGroup::new(open).secondary(ask)
        }
        _ => ActionGroup::new(ask),
    })
}

fn run_history(ui: &Ui, door: &DoorState) -> Element {
    let mut rows: Vec<Element> = Vec::new();
    for run in door.runs.iter().take(20) {
        let secondary = match &run.status {
            RunStatus::Running => String::from("Running…"),
            RunStatus::Completed(Some(value)) => editor::literal_text(value),
            RunStatus::Completed(None) => String::from("Finished"),
            RunStatus::Failed(reason) => format!("Stopped: {reason}"),
        };
        let open = {
            let ui = ui.clone();
            let target = format!("{SAVED}{}", run.shortcut_id);
            move || ui.open(target.clone())
        };
        let mut item = ListRow::new(format!("run-record-{}", run.serial), run.name.clone())
            .secondary(secondary)
            .meta(run.started.clone())
            .on_activate(open);
        item = match &run.status {
            RunStatus::Running => item.active(),
            RunStatus::Failed(_) => item.error(),
            // Every row carries a dot, so the times line up down the list.
            RunStatus::Completed(_) => item.idle(),
        };
        rows.push(Box::new(item));
    }
    Box::new(Section::titled(ui::i18n!("shortcuts-runs"), rows))
}

// ------------------------------------------------------------ saved shortcut

/// What the last run of `shortcut` came to, in words.
fn last_run_text(door: &DoorState, shortcut: &str) -> String {
    let Some(run) = door.last_run(shortcut) else {
        return String::from("Not run yet");
    };
    match &run.status {
        RunStatus::Running => format!("Running since {}", run.started),
        RunStatus::Completed(_) => format!("Finished at {}", run.started),
        RunStatus::Failed(reason) => format!("Stopped at {}: {reason}", run.started),
    }
}

/// Run shortcut `id` with what its input fields hold.
fn run_button(ui: &Ui, id: &str, fields: &[(ActionParam, LocalState<String>)]) -> Button {
    let button = Button::new(&format!("run-{id}"), "shortcuts-run").role(ButtonRole::Primary);
    if fields.is_empty() {
        return ui.door_button(
            button,
            &DoorEffect::Run {
                shortcut: id.to_owned(),
                inputs: None,
            },
        );
    }
    let run_id = id.to_owned();
    let values = fields.to_vec();
    ui.actions.bind_effect(
        button,
        DoorEffect::RUN.map(move |()| {
            let inputs: serde_json::Map<String, Value> = values
                .iter()
                .map(|(input, value)| {
                    (input.name.to_string(), editor::literal(&value.get(), input))
                })
                .collect();
            (run_id.clone(), Some(inputs))
        }),
    )
}

fn saved_page(ui: &Ui, input: &EditorInput, shortcut: &Shortcut) -> Element {
    let id = shortcut.id.clone();
    let fields: Vec<(ActionParam, LocalState<String>)> = shortcut
        .inputs
        .iter()
        .map(|input| (input.clone(), LocalState::new(String::new())))
        .collect();
    let run = run_button(ui, &id, &fields);
    let edit = ui.bind(
        Button::new(&format!("edit-{id}"), "shortcuts-edit"),
        Workbench::OPEN_SHORTCUT.map({
            let json = serde_json::to_value(shortcut).unwrap_or(Value::Null);
            move |()| json.clone()
        }),
    );
    let delete = ui.door_button(
        Button::new(&format!("delete-{id}"), "shortcuts-delete").role(ButtonRole::Destructive),
        &DoorEffect::Delete {
            shortcut: id.clone(),
        },
    );

    let mut items: Vec<Element> = vec![
        Box::new(
            Text::new(&shortcut.name)
                .with_role(TextRole::Title)
                .with_heading_level(1),
        ),
        saved_facts(ui, shortcut),
    ];
    if !fields.is_empty() {
        let mut form: Vec<Element> = Vec::new();
        for (param, value) in &fields {
            form.push(Box::new(labeled_field(
                param.description.to_string(),
                TextInput::from_state(value)
                    .with_id(&format!("run-{id}-input-{}", param.name))
                    .with_accessible_name(param.description.to_string())
                    .with_placeholder("shortcuts-input-placeholder"),
            )));
        }
        items.push(Box::new(Section::titled(
            ui::i18n!("shortcuts-inputs"),
            form,
        )));
    }
    // Deleting sits apart from the actions that carry on with the shortcut.
    items.push(Box::new(
        Row::new()
            .with_justify(ui::taffy::JustifyContent::SpaceBetween)
            .push(Box::new(ActionGroup::new(delete)))
            .push(Box::new(ActionGroup::new(run).secondary(edit))),
    ));

    let numbered: Vec<Element> = shortcut
        .steps
        .iter()
        .map(|step| {
            let number = editor::step_number(&shortcut.steps, &step.id).unwrap_or(0);
            let title = editor::title(step, &input.catalog, &shortcut.steps);
            let line = match editor::context(step, &input.catalog) {
                Some(context) => format!("{number}. {title}, {context}"),
                None => format!("{number}. {title}"),
            };
            Box::new(Text::new(&line)) as Element
        })
        .collect();
    if !numbered.is_empty() {
        items.push(Box::new(Section::titled(
            ui::i18n!("shortcuts-steps"),
            numbered,
        )));
    }
    page(ui, items)
}

/// What is known about a saved shortcut, following its runs.
fn saved_facts(ui: &Ui, shortcut: &Shortcut) -> Element {
    let id = shortcut.id.clone();
    Box::new(ModelView::from_state_by(
        &ui.door,
        {
            let id = id.clone();
            move |door| last_run_text(door, &id)
        },
        move |door| -> Element {
            Box::new(Section::titled(
                ui::i18n!("shortcuts-last-run"),
                vec![Box::new(Text::new(&last_run_text(door, &id)))],
            ))
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
            // capture, the first frame) shows the current page.
            if let Ok(mut follow) = follow.try_borrow_mut() {
                follow();
            }
            editor.with(EditorInput::structure)
        },
        move || {
            let content = builder.editor.with(|input| build_page(&builder, input));
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
    Box::new(
        Stack::new()
            .push(Box::new(body))
            .push(Box::new(Sheet::new(ui))),
    )
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
            part: ui::widget::optional(None),
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
type SheetKey = (
    Option<(Id, Vec<Permission>, Vec<tilo_automation::TriggerSpec>)>,
    BTreeMap<String, Vec<String>>,
);

type SaveDialog = ModalDialog<ModelView<SheetKey>>;

/// The save sheet: the shared dialog, shown while its state says so. When
/// the checker finds problems the sheet lists them and offers no save.
struct Sheet {
    dialog: SaveDialog,
    open: LocalState<bool>,
    blocked: bool,
    editor: LocalState<EditorInput>,
    make: Box<dyn Fn(bool) -> SaveDialog>,
}

impl Sheet {
    fn new(ui: &Ui) -> Self {
        let builder = ui.clone();
        let make: Box<dyn Fn(bool) -> SaveDialog> =
            Box::new(move |blocked| save_dialog(&builder, blocked));
        Self {
            dialog: make(false),
            open: ui.sheet.clone(),
            blocked: false,
            editor: ui.editor.clone(),
            make,
        }
    }
}

fn save_dialog(ui: &Ui, blocked: bool) -> SaveDialog {
    let sheet_builder = ui.clone();
    let body = ModelView::from_state_by(
        &ui.editor,
        |input| {
            (
                input.draft.as_ref().map(|(id, draft)| {
                    (
                        id.clone(),
                        grant::least_privilege(&draft.program().steps, &input.catalog),
                        draft.program().triggers,
                    )
                }),
                input.problems.clone(),
            )
        },
        move |input| grant_body(&sheet_builder, input),
    );
    let close = {
        let sheet = ui.sheet.clone();
        move || sheet.set(false)
    };
    let mut dialog = ModalDialog::new(ui::i18n!("shortcuts-grant-title"), body)
        .with_test_id("shortcuts-save-sheet")
        .on_dismiss(close.clone())
        .with_max_width(560.0);
    if !blocked {
        let ui = ui.clone();
        dialog = dialog
            .primary_action(ui::i18n!("shortcuts-save"), move || save_open_draft(&ui))
            .secondary_action(ui::i18n!("shortcuts-cancel"), close);
    }
    dialog
}

/// Save the draft that is open, as the save operation does.
fn save_open_draft(ui: &Ui) {
    let Some(draft) = ui
        .editor
        .with(|input| input.draft.as_ref().map(|(id, _)| id.as_str().to_owned()))
    else {
        return;
    };
    let Ok(mut core) = ui.core.try_borrow_mut() else {
        return;
    };
    if let Err(reason) = core.save(&draft, None) {
        ui.door
            .update(|state| state.notice = Some(Notice::Failed(reason)));
        ui.sheet.set(false);
    }
}

impl ui::widget::Decorator for Sheet {
    type Inner = SaveDialog;

    fn inner(&self) -> &SaveDialog {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut SaveDialog {
        &mut self.dialog
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let blocked = self.editor.with(|input| !input.problems.is_empty());
        if blocked != self.blocked {
            self.dialog = (self.make)(blocked);
            self.blocked = blocked;
        }
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

/// The page of what is open.
fn build_page(ui: &Ui, input: &EditorInput) -> Element {
    if input.draft.is_some() {
        return build_editor(ui, input);
    }
    if let Some(shortcut) = input
        .open_saved
        .as_ref()
        .and_then(|id| input.saved.iter().find(|saved| &saved.id == id))
    {
        return saved_page(ui, input, shortcut);
    }
    overview_page(ui)
}

fn build_editor(ui: &Ui, input: &EditorInput) -> Element {
    let Some((id, draft)) = &input.draft else {
        return overview_page(ui);
    };
    let draft_id = id.clone();
    let name = match ui.actions.edit_binding(
        Workbench::paths().drafts.item(&draft_id).name,
        |path, name: String| path.replace(name),
    ) {
        Some(binding) => TextInput::local().on_action(binding),
        None => TextInput::local().disabled(true),
    }
    .with_id("draft-name")
    .with_accessible_name("shortcuts-name")
    .with_initial_value(&draft.name);
    let review = {
        let sheet = ui.sheet.clone();
        Button::new("review-save", "shortcuts-review")
            .role(ButtonRole::Primary)
            .with_enabled(save_blocker(input).is_none())
            .on_view(move || sheet.set(true))
    };
    let discard = ui.bind(
        Button::new("discard-draft", "shortcuts-discard-button").role(ButtonRole::Destructive),
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
                (_, 0) => save_blocker(input).unwrap_or_else(|| String::from("Ready to save")),
                (Some(top), _) => top.join("; "),
                (None, 1) => String::from("1 step needs attention"),
                (None, count) => format!("{count} problems to fix before saving"),
            }
        })
        .with_id("draft-problems")
        .with_role(TextRole::Caption)
        .with_live(ui::a11y::Politeness::Polite)
    };
    let status = Row::new()
        .with_justify(ui::taffy::JustifyContent::SpaceBetween)
        .push(Box::new(problems))
        .push(Box::new(ActionGroup::new(review).secondary(discard)));
    let mut items: Vec<Element> = vec![
        Box::new(labeled_field("shortcuts-name", name)),
        Box::new(status),
    ];
    if let Loaded::Failed(reason) = &input.catalogue {
        items.push(Box::new(
            Banner::warning(ui::i18n!("shortcuts-catalogue-unavailable"))
                .with_message(reason.clone()),
        ));
    }
    items.push(schedule_section(ui, &draft_id));
    items.push(inputs_section(ui, &draft_id, draft));
    items.push(Box::new(Section::titled(
        ui::i18n!("shortcuts-steps"),
        vec![block(ui, input, &draft_id, draft, "", 0)],
    )));
    items.push(output_section(ui, input, &draft_id, draft));
    page(ui, items)
}

/// Why the open draft cannot be saved yet besides what the checker finds:
/// it does nothing, or the actions its steps choose from are not known here.
pub(crate) fn save_blocker(input: &EditorInput) -> Option<String> {
    let (_, draft) = input.draft.as_ref()?;
    if draft.program().steps.is_empty() {
        return Some(String::from("Add a step to save this shortcut"));
    }
    match &input.catalogue {
        Loaded::Loading => Some(String::from("Reading the actions your apps offer…")),
        Loaded::Failed(_) => Some(String::from(
            "Can't save here: the actions your apps offer could not be read",
        )),
        Loaded::Ready(()) => None,
    }
}

/// Scheduling edits only this draft; the existing save sheet reviews authority.
fn schedule_section(ui: &Ui, draft_id: &Id) -> Element {
    let id = draft_id.clone();
    let controls_id = draft_id.clone();
    let builder = ui.clone();
    Box::new(ModelView::from_state_by(
        &ui.editor,
        move |input| {
            input
                .draft
                .as_ref()
                .filter(|(draft, _)| draft == &id)
                .map(|(_, draft)| draft.program().triggers)
        },
        move |input| {
            let time = input
                .draft
                .as_ref()
                .filter(|(id, _)| id == &controls_id)
                .and_then(|(_, draft)| {
                    let triggers = draft.program().triggers;
                    triggers.iter().find_map(|trigger| match trigger {
                        tilo_automation::TriggerSpec::Schedule {
                            schedule: tilo_automation::Schedule::Calendar { at, weekdays },
                            ..
                        } if weekdays.is_empty() => Some(*at),
                        _ => None,
                    })
                });
            schedule_controls(&builder, controls_id.clone(), time)
        },
    ))
}
fn schedule_controls(ui: &Ui, draft_id: Id, time: Option<tilo_automation::TimeOfDay>) -> Element {
    let time = time.unwrap_or(tilo_automation::TimeOfDay { hour: 8, minute: 0 });
    let enabled = LocalState::new(time_is_enabled(ui, &draft_id));
    let hour = LocalState::new(format!("{:02}", time.hour));
    let minute = LocalState::new(format!("{:02}", time.minute));
    let checkbox = ui.bind(
        Checkbox::from_state(&enabled)
            .with_id("schedule-enabled")
            .with_label("shortcuts-schedule-enabled"),
        Workbench::SET_SCHEDULE.map({
            let draft = draft_id.clone();
            let hour = hour.clone();
            let minute = minute.clone();
            move |enabled: bool| {
                (
                    draft.clone(),
                    enabled,
                    hour.get().parse::<i64>().unwrap_or(8),
                    minute.get().parse::<i64>().unwrap_or(0),
                )
            }
        }),
    );
    let hour_control = ui.bind(
        Dropdown::from_state((0..24).map(|hour| format!("{hour:02}")).collect(), &hour)
            .with_id("schedule-hour")
            .with_accessible_name("shortcuts-schedule-hour"),
        Workbench::SET_SCHEDULE.map({
            let draft = draft_id.clone();
            let enabled = enabled.clone();
            let minute = minute.clone();
            move |hour: String| {
                (
                    draft.clone(),
                    enabled.get(),
                    hour.parse::<i64>().unwrap_or(8),
                    minute.get().parse::<i64>().unwrap_or(0),
                )
            }
        }),
    );
    let minute_control = ui.bind(
        Dropdown::from_state(
            (0..60).map(|minute| format!("{minute:02}")).collect(),
            &minute,
        )
        .with_id("schedule-minute")
        .with_accessible_name("shortcuts-schedule-minute"),
        Workbench::SET_SCHEDULE.map({
            move |minute: String| {
                (
                    draft_id.clone(),
                    enabled.get(),
                    hour.get().parse::<i64>().unwrap_or(8),
                    minute.parse::<i64>().unwrap_or(0),
                )
            }
        }),
    );
    Box::new(Section::titled(
        ui::i18n!("shortcuts-schedule"),
        vec![
            Box::new(checkbox),
            Box::new(labeled_field("shortcuts-schedule-hour", hour_control)),
            Box::new(labeled_field("shortcuts-schedule-minute", minute_control)),
            Box::new(
                Text::new(&ui::i18n!("shortcuts-schedule-description"))
                    .with_role(TextRole::Caption),
            ),
        ],
    ))
}
fn time_is_enabled(ui: &Ui, draft_id: &Id) -> bool {
    ui.editor.with(|input| {
        input
            .draft
            .as_ref()
            .filter(|(id, _)| id == draft_id)
            .is_some_and(|(_, draft)| {
                draft
                    .program()
                    .triggers
                    .iter()
                    .any(|trigger| matches!(trigger, tilo_automation::TriggerSpec::Schedule { schedule: tilo_automation::Schedule::Calendar { weekdays, .. }, .. } if weekdays.is_empty()))
            })
    })
}

fn inputs_section(ui: &Ui, draft_id: &Id, draft: &Draft) -> Element {
    let mut items: Vec<Element> = Vec::new();
    for input in &draft.program().inputs {
        let remove = ui
            .bind(
                Button::icon_only(
                    &format!("remove-input-{}", input.name),
                    Icon::X,
                    &ui::i18n!("shortcuts-remove"),
                ),
                Workbench::REMOVE_INPUT.map({
                    let draft = draft_id.clone();
                    let name = input.name.to_string();
                    move |()| (draft.clone(), name.clone())
                }),
            )
            .control_size(ControlSize::Small);
        items.push(Box::new(
            Row::new()
                .push(Box::new(
                    Text::new(&input.name).with_kind(TextKind::Identifier),
                ))
                .push(Box::new(caption(editor::param_type_label(input.ty))))
                .push(Box::new(Spacer::new()))
                .push(Box::new(remove)),
        ));
    }
    let name = LocalState::new(String::new());
    let kind = LocalState::new(String::from("text"));
    let add = ui.bind(
        Button::new("add-input", "shortcuts-add-input-button").control_size(FIELD_HEIGHT),
        Workbench::ADD_INPUT.map({
            let draft = draft_id.clone();
            let name = name.clone();
            let kind = kind.clone();
            move |()| (draft.clone(), name.get().trim().to_owned(), kind.get())
        }),
    );
    items.push(Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(
                TextInput::from_state(&name)
                    .with_id("new-input-name")
                    .with_accessible_name("shortcuts-input-name")
                    .with_placeholder("shortcuts-input-name"),
            ))
            .push(Box::new({
                // The stored value stays the type's name; the choice reads
                // as words.
                let mut types = Dropdown::from_state(
                    INPUT_TYPES.iter().map(|ty| (*ty).to_owned()).collect(),
                    &kind,
                )
                .with_id("new-input-type")
                .with_accessible_name("shortcuts-input-type")
                .control_size(FIELD_HEIGHT);
                types.set_value_options(
                    INPUT_TYPES
                        .iter()
                        .map(|ty| {
                            let label =
                                crate::model::param_type(ty).map_or(*ty, editor::param_type_label);
                            ((*ty).to_owned(), label.to_owned())
                        })
                        .collect(),
                );
                types
            }))
            .push(Box::new(add)),
    ));
    Box::new(Section::titled(ui::i18n!("shortcuts-inputs"), items))
}

fn output_section(ui: &Ui, input: &EditorInput, draft_id: &Id, draft: &Draft) -> Element {
    let program = &draft.program();
    let values = values_at(
        &editor::candidate(draft, &input.catalog),
        &input.catalog,
        &editor::step_path(&program.steps.len().to_string()),
    );
    let choices: Vec<Choice> = values
        .into_iter()
        .map(|option| Choice {
            label: editor::describe(&option.binding, &program.steps),
            binding: option.binding,
        })
        .collect();
    let (values, labels, current) =
        choice_options(&choices, program.output.as_ref(), false, &program.steps);
    let state = LocalState::new(current);
    let mut picker = Dropdown::from_state(labels, &state)
        .with_id("draft-output")
        .with_accessible_name("shortcuts-output")
        .control_size(FIELD_HEIGHT);
    picker.set_value_options(values);
    let picker = ui.edit(
        picker,
        Workbench::paths().drafts.item(draft_id).output,
        |value: String| binding_json(&value).and_then(|value| serde_json::from_value(value).ok()),
    );
    Box::new(Section::titled(
        ui::i18n!("shortcuts-output"),
        vec![Box::new(picker)],
    ))
}

/// `(value, label)` pairs for a binding dropdown, the plain labels, and the
/// value of `current`. `steps` is the program the bindings belong to.
fn choice_options(
    choices: &[Choice],
    current: Option<&Binding>,
    literal: bool,
    steps: &[Step],
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
        values.push((selected.clone(), editor::describe(binding, steps)));
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
    let program = draft.program();
    let steps = block_at(&program.steps, address).unwrap_or_default();
    let mut items: Vec<Element> = Vec::new();
    if steps.is_empty() && depth == 0 {
        items.push(Box::new(caption("shortcuts-steps-empty")));
    }
    for (index, step) in steps.iter().enumerate() {
        let path = if address.is_empty() {
            index.to_string()
        } else {
            format!("{address}.{index}")
        };
        items.push(step_card(
            ui,
            input,
            draft_id,
            draft,
            step,
            &path,
            (index, steps.len()),
        ));
    }
    items.push(add_controls(ui, input, draft_id, address));
    Box::new(Section::new(items))
}

fn add_controls(ui: &Ui, input: &EditorInput, draft_id: &Id, address: &str) -> Element {
    let key = if address.is_empty() { "top" } else { address };
    if input.picker.as_deref() == Some(address) {
        return action_picker(ui, input, draft_id, address);
    }
    let picker = ui.picker.clone();
    let address = address.to_owned();
    Box::new(
        Row::new().push(Box::new(
            Button::new(&format!("add-action-{key}"), "shortcuts-add-step-button")
                .on_view(move || picker.set(Some(address.clone()))),
        )),
    )
}

/// One kind of step that is not an app's action, as the picker offers it:
/// what it is called, what it does, and the step it adds.
struct BlockOffer {
    id: &'static str,
    label: &'static str,
    detail: &'static str,
    step: Value,
}

fn block_offers() -> Vec<BlockOffer> {
    vec![
        BlockOffer {
            id: "if",
            label: "If",
            detail: "Do some steps only when a value is set",
            step: json!({"step": "if", "cond": {"truthy": {"literal": true}}, "then": []}),
        },
        BlockOffer {
            id: "repeat",
            label: "Repeat with each",
            detail: "Do some steps for every item in a list",
            step: json!({"step": "for_each", "items": {"literal": []}, "body": []}),
        },
        BlockOffer {
            id: "set",
            label: "Set variable",
            detail: "Keep a value to use in later steps",
            step: json!({"step": "set_var", "name": "value", "value": {"literal": ""}}),
        },
        BlockOffer {
            id: "ask",
            label: "Ask for a value",
            detail: "Ask whoever runs the shortcut for something",
            step: json!({"step": "ask", "param": {
                "name": "answer", "ty": "text", "required": true, "description": "Your answer"
            }}),
        },
        BlockOffer {
            id: "ai",
            label: "Summarise with the agent",
            detail: "Have the agent write a short summary of some text",
            step: json!({"step": "ai", "transform": "summarise", "input": {"literal": ""}}),
        },
    ]
}

/// The live catalogue, grouped by app, each action with what it takes.
fn action_picker(ui: &Ui, input: &EditorInput, draft_id: &Id, address: &str) -> Element {
    let close = {
        let picker = ui.picker.clone();
        Button::new(
            &format!(
                "close-picker-{}",
                if address.is_empty() { "top" } else { address }
            ),
            "shortcuts-close",
        )
        .role(ButtonRole::Plain)
        .on_view(move || picker.set(None))
    };
    let mut groups: Vec<Element> = Vec::new();
    let blocks: Vec<Element> = block_offers()
        .into_iter()
        .map(|offer| {
            let mut row =
                ListRow::new(format!("offer-{}", offer.id), offer.label).secondary(offer.detail);
            if let Some(binding) = ui.actions.invocation(Workbench::ADD_STEP.with((
                draft_id.clone(),
                address.to_owned(),
                offer.step,
            ))) {
                row = row.on_action(binding);
            }
            Box::new(row) as Element
        })
        .collect();
    groups.push(Box::new(
        Section::new(blocks).with_header(SectionHeader::group(ui::i18n!("shortcuts-blocks"))),
    ));
    for group in editor::picker(&input.catalog) {
        let mut rows: Vec<Element> = Vec::new();
        for kind in group.kinds {
            for offer in kind.offers {
                let step = serde_json::to_value(offer.step()).unwrap_or(Value::Null);
                let secondary = if offer.needs.is_empty() {
                    offer.resource_label.clone()
                } else {
                    format!(
                        "{}, needs {}",
                        offer.resource_label,
                        offer.needs.join(" and ")
                    )
                };
                let mut row = ListRow::new(
                    format!("offer-{}-{}-{}", offer.app_id, offer.resource, offer.action),
                    offer.label.clone(),
                )
                .secondary(secondary);
                if let Some(binding) = ui.actions.invocation(Workbench::ADD_STEP.with((
                    draft_id.clone(),
                    address.to_owned(),
                    step,
                ))) {
                    row = row.on_action(binding);
                }
                rows.push(Box::new(row));
            }
        }
        groups.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group(group.label)),
        ));
    }
    if groups.is_empty() {
        groups.push(Box::new(caption(match input.catalogue {
            Loaded::Loading => "shortcuts-catalogue-loading",
            Loaded::Failed(_) => "shortcuts-catalogue-unavailable",
            Loaded::Ready(()) => "shortcuts-catalogue-empty",
        })));
    }
    Box::new(
        Section::new(groups).with_header(
            SectionHeader::heading(ui::i18n!("shortcuts-add-step-button")).action(close),
        ),
    )
}

/// One step: a card holding what it does, with its own actions in the
/// header, and the blocks inside it indented below.
fn step_card(
    ui: &Ui,
    input: &EditorInput,
    draft_id: &Id,
    draft: &Draft,
    step: &Step,
    path: &str,
    (index, count): (usize, usize),
) -> Element {
    let program = draft.program();
    let steps = &program.steps;
    let at = StepEditor {
        ui,
        input,
        draft_id,
        draft,
        path,
    };
    let number = editor::step_number(steps, &step.id).unwrap_or(0);
    let title = format!("{number}. {}", editor::title(step, &input.catalog, steps));
    let actions = step_actions(ui, draft_id, path, (index, count));
    let (body, nested) = step_fields(&at, step);
    let card = Card::new(Box::new(body)).with_title_action(title, Box::new(actions));
    let mut items: Vec<Element> = vec![Box::new(card)];
    items.extend(nested);
    Box::new(Section::new(items))
}

/// The steps inside a step, indented under it and named by what they are
/// for.
fn branch(title: String, block: Element) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .align_start()
            .push(Box::new(Spacer::token(ui::tokens::SpacingStep::S6)))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(
                        Section::new(vec![block]).with_header(SectionHeader::group(title)),
                    )),
            )),
    )
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
        Workbench::REPLACE_STEP.map({
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
    let program = &draft.program();
    let shortcut = editor::candidate(draft, &input.catalog);
    let choices: Vec<Choice> = values_at(&shortcut, &input.catalog, &editor::step_path(path))
        .into_iter()
        .map(|option| Choice {
            label: editor::describe(&option.binding, &program.steps),
            binding: option.binding,
        })
        .collect();
    let (values, labels, selected) = choice_options(&choices, current, true, &program.steps);
    let state = LocalState::new(selected.clone());
    let mut picker = Dropdown::from_state(labels, &state)
        .with_id(&format!("step-{suffix}-source-{path}"))
        .with_accessible_name(label)
        .control_size(FIELD_HEIGHT);
    picker.set_value_options(values);
    let rewrite = std::sync::Arc::new(rewrite);
    let picker = ui.bind(
        picker,
        Workbench::REPLACE_STEP.map({
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
            Workbench::REPLACE_STEP.map({
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
    let described = param.description.to_string();
    let label = format!(
        "{}{}",
        if described.is_empty() {
            &name
        } else {
            &described
        },
        if param.required { "" } else { " (optional)" },
    );
    let choices = editor::choices(draft, &input.catalog, path, param);
    let (values, labels, selected) =
        choice_options(&choices, current, true, &draft.program().steps);
    let state = LocalState::new(selected);
    let mut source = Dropdown::from_state(labels, &state)
        .with_id(&format!("arg-{path}-{name}"))
        .with_accessible_name(label.clone())
        .control_size(FIELD_HEIGHT);
    source.set_value_options(values);
    let source = ui.bind(
        source,
        Workbench::BIND_ARGUMENT.map({
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
                Workbench::BIND_ARGUMENT.map(move |on: bool| {
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
                    Workbench::BIND_ARGUMENT.map(move |number: f64| {
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
                    Workbench::BIND_ARGUMENT.map(move |text: String| {
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
fn grant_body(_ui: &Ui, input: &EditorInput) -> Element {
    let Some((_, draft)) = &input.draft else {
        return ui::widget::optional(None);
    };
    let permissions: Vec<Permission> =
        grant::least_privilege(&draft.program().steps, &input.catalog);
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(
            Text::new("shortcuts-grant-explain").with_id("shortcuts-grant-explain"),
        ));
    for (index, trigger) in draft.program().triggers.iter().enumerate() {
        if let tilo_automation::TriggerSpec::Schedule {
            schedule: tilo_automation::Schedule::Calendar { at, .. },
            catch_up,
        } = trigger
        {
            body = body.push(Box::new(
                Text::with_args(
                    "shortcuts-schedule-review",
                    &[(
                        "time",
                        ui::i18n::ArgValue::from(ui::clock::hour_minute(
                            at.hour.into(),
                            at.minute.into(),
                        )),
                    )],
                )
                .with_role(TextRole::Caption)
                .with_id(format!("schedule-review-{index}")),
            ));
            body = body.push(Box::new(caption(match catch_up {
                tilo_automation::CatchUp::RunOnce => "shortcuts-schedule-missed-once",
                tilo_automation::CatchUp::Skip => "shortcuts-schedule-missed-skip",
            })));
        }
    }
    if permissions.is_empty() {
        body = body.push(Box::new(caption("shortcuts-grant-none")));
    } else {
        for permission in &permissions {
            body = body.push(Box::new(Text::new(&format!(
                "{} ({})",
                permission.label,
                permission.kind.phrase()
            ))));
        }
    }
    if !input.problems.is_empty() {
        body = body.push(Box::new(
            Banner::warning(ui::i18n!("shortcuts-grant-blocked")).with_message(
                editor::problem_lines(&draft.program().steps, &input.problems).join("\n"),
            ),
        ));
    }
    Box::new(body)
}

/// A step's own actions: move it up or down its block, or remove it.
fn step_actions(ui: &Ui, draft_id: &Id, path: &str, (index, count): (usize, usize)) -> ActionGroup {
    let step_button = |id: &str, icon: Icon, name: &str| {
        Button::icon_only(&format!("{id}-{path}"), icon, name).control_size(ControlSize::Small)
    };
    let up = ui
        .bind(
            step_button("step-up", Icon::ChevronUp, &ui::i18n!("shortcuts-move-up")),
            Workbench::MOVE_STEP.map({
                let draft = draft_id.clone();
                let path = path.to_owned();
                move |()| (draft.clone(), path.clone(), true)
            }),
        )
        .with_enabled(index > 0);
    let down = ui
        .bind(
            step_button(
                "step-down",
                Icon::ChevronDown,
                &ui::i18n!("shortcuts-move-down"),
            ),
            Workbench::MOVE_STEP.map({
                let draft = draft_id.clone();
                let path = path.to_owned();
                move |()| (draft.clone(), path.clone(), false)
            }),
        )
        .with_enabled(index + 1 < count);
    let remove = ui.bind(
        step_button("step-remove", Icon::Trash2, &ui::i18n!("shortcuts-remove")),
        Workbench::REMOVE_STEP.map({
            let draft = draft_id.clone();
            let path = path.to_owned();
            move |()| (draft.clone(), path.clone())
        }),
    );
    ActionGroup::new(remove).secondary(up).secondary(down)
}

/// What a step is made of: its fields in a column, and the blocks inside it.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per step kind, read top to bottom like the editor"
)]
fn step_fields(at: &StepEditor<'_>, step: &Step) -> (Column, Vec<Element>) {
    let StepEditor {
        ui,
        input,
        draft_id,
        draft,
        path,
    } = *at;
    let mut body = Column::new().with_intrinsic_height().with_no_padding();
    if let Some(context) = editor::context(step, &input.catalog) {
        body = body.push(Box::new(caption(&context)));
    }
    // What the checker found with this step, one notice each; the card is
    // rebuilt when that changes, so a step with nothing to report has none.
    for reason in input.problems.get(path).into_iter().flatten() {
        body = body.push(Box::new(Banner::warning(reason.clone())));
    }
    let mut nested: Vec<Element> = Vec::new();
    match &step.kind {
        StepKind::Invoke { args, .. } => {
            for param in editor::params_of(step, &input.catalog) {
                body = body.push(argument_row(
                    ui,
                    input,
                    draft_id,
                    draft,
                    path,
                    &param,
                    args.get(param.name.as_ref()),
                ));
            }
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
            body = body.push(value_row(at, "condition", "shortcuts-condition", current, {
                let then = then.clone();
                let otherwise = otherwise.clone();
                move |binding| json!({"step": "if", "cond": {"truthy": binding}, "then": then, "else": otherwise})
            }));
            nested.push(branch(
                ui::i18n!("shortcuts-then"),
                block(ui, input, draft_id, draft, &format!("{path}.then"), 1),
            ));
            nested.push(branch(
                ui::i18n!("shortcuts-otherwise"),
                block(ui, input, draft_id, draft, &format!("{path}.else"), 1),
            ));
        }
        StepKind::ForEach {
            items,
            item,
            body: repeated,
        } => {
            body = body.push(value_row(at, "items", "shortcuts-items", Some(items), {
                let item = item.clone();
                let repeated = repeated.clone();
                move |binding| json!({"step": "for_each", "items": binding, "item": item, "body": repeated})
            }));
            body = body.push(Box::new(caption(&format!(
                "Inside the repeat, each item is the variable {item}"
            ))));
            nested.push(branch(
                ui::i18n!("shortcuts-repeat"),
                block(ui, input, draft_id, draft, &format!("{path}.each"), 1),
            ));
        }
        StepKind::SetVar { name, value } => {
            body = body.push(step_text(
                at,
                "variable-name",
                "shortcuts-variable",
                name,
                {
                    let value = value.clone();
                    move |text| json!({"step": "set_var", "name": text, "value": value})
                },
            ));
            body = body.push(value_row(at, "value", "shortcuts-value", Some(value), {
                let name = name.clone();
                move |binding| json!({"step": "set_var", "name": name, "value": binding})
            }));
        }
        StepKind::Ask { param } => {
            body = body.push(step_text(
                at,
                "question",
                "shortcuts-question",
                &param.description,
                {
                    let param = param.clone();
                    move |text| {
                        let mut param = param.clone();
                        param.description = text.into();
                        json!({"step": "ask", "param": param})
                    }
                },
            ));
        }
        StepKind::Ai {
            transform,
            input: source,
        } => {
            body = body.push(value_row(
                at,
                "ai-input",
                "shortcuts-ai-input",
                Some(source),
                {
                    let transform = transform.clone();
                    move |binding| json!({"step": "ai", "transform": transform, "input": binding})
                },
            ));
            if let AiTransform::Freeform { prompt } = transform {
                body = body.push(step_text(at, "ai-prompt", "shortcuts-ai-prompt", prompt, {
                    let source = source.clone();
                    move |text| json!({"step": "ai", "transform": {"freeform": {"prompt": text}}, "input": source})
                }));
            }
        }
    }
    (body, nested)
}
