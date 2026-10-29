//! A project's or an area's page: the tasks that work towards it, the
//! documents those tasks carry, and the people involved in it.
//!
//! The project is an ordinary resource above the tasks ([`crate::model`]):
//! it stores no task of its own, and its tasks are the ones whose `area`
//! names it, however deep under their parent they sit. So the page reads the
//! same issues the list does, and nothing here is a second copy. Who is
//! involved is the project's own to keep, with the role each person has;
//! saying so grants nothing, because sharing is the framework's.

use ui::prelude::*;
use ui::tilo_resource::NonBlank;
use ui::widget::{Fact, Facts};

use crate::model::{Kind, Tracker, tracker};
use crate::{reference, tr};

/// The project's page, or that it is gone.
pub(crate) fn page(cx: &ViewCx<Tracker>, id: Id) -> Element {
    let exists = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.areas.contains(&id)
    });
    cx.switch(&exists, move |cx, exists| {
        if *exists {
            content(cx, &id)
        } else {
            Box::new(
                Column::new()
                    .with_rhythm(ui::tokens::Rhythm::Group)
                    .push(Box::new(
                        EmptyState::new("tasks-project-gone-title")
                            .with_message("tasks-project-gone-message")
                            .with_id("tasks-project-gone"),
                    )),
            )
        }
    })
}

fn content(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let paths = Tracker::paths().areas.item(id);
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Container::new()
                    .readable()
                    .centered()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_rhythm(ui::tokens::Rhythm::Group)
                            .push(Box::new(
                                cx.input(paths.name)
                                    .with_id("tasks-project-name")
                                    .with_accessible_name(tr("tasks-field-project-name"))
                                    .with_plain_chrome()
                                    .with_text_role(TextRole::Title),
                            ))
                            .push(kind(cx, id))
                            .push(people(cx, id))
                            .push(tasks(cx, id))
                            .push(documents(cx, id)),
                    )),
            )),
    )
}

/// What the project says it is: a project with an end, or an area that goes
/// on. It is changed through the tracker's own kind verb, so automation and
/// a screen reader say the same as the dropdown does.
fn kind(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let read = id.clone();
    let edit = id.clone();
    let current = move |tracker: &Tracker| {
        tr(tracker
            .area(&read)
            .map_or(Kind::Project, |area| area.kind)
            .label_key())
    };
    let picker = cx.control_with(
        |state| {
            Dropdown::from_state(
                Kind::ALL.iter().map(|kind| tr(kind.label_key())).collect(),
                state,
            )
            .with_id("tasks-project-kind")
            .with_accessible_name(tr("tasks-field-kind"))
        },
        current,
        Tracker::SET_KIND.map(move |label: String| tracker::SetKind {
            area: edit.clone(),
            kind: Kind::ALL
                .into_iter()
                .find(|kind| tr(kind.label_key()) == label)
                .unwrap_or_default(),
        }),
    );
    let kind = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.area(&id).map_or(Kind::Project, |area| area.kind)
    });
    let note = cx.switch(&kind, |_cx, kind| -> Element {
        Box::new(
            Text::new(match kind {
                Kind::Project => "tasks-kind-project-note",
                Kind::Area => "tasks-kind-area-note",
            })
            .dimmed(),
        ) as Element
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(picker))
            .push(note),
    )
}

/// The people the project says are involved, and the role each has. The
/// list grants nothing: sharing a project is sharing it as any resource.
fn people(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let people = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .area(&id)
                .map(|area| {
                    area.people
                        .values()
                        .map(|person| (person.name.clone(), person.role.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    });
    let area = id.clone();
    let draft = cx.local(String::new());
    let role = cx.local(String::new());
    let name_field = TextInput::from_state(&draft)
        .with_id("tasks-project-person-name")
        .with_accessible_name(tr("tasks-field-person"))
        .with_placeholder("tasks-project-person-placeholder");
    let role_field = TextInput::from_state(&role)
        .with_id("tasks-project-person-role")
        .with_accessible_name(tr("tasks-field-role"))
        .with_placeholder("tasks-project-role-placeholder");
    // Someone is involved by name: the button waits for one. The role is
    // read as it stands when they are added, and cleared with the name.
    let involve = Tracker::INVOLVE.try_map({
        let role = role.clone();
        move |name: String| {
            let role_text = role.get();
            Some(tracker::Involve {
                area: area.clone(),
                name: NonBlank::parse(&name)?,
                role: (!role_text.trim().is_empty()).then_some(role_text),
            })
        }
    });
    let button = cx.actions().bind_draft_clearing(
        Button::new("tasks-project-involve", "tasks-project-involve")
            .with_icon(ui::icons::Icon::Plus),
        &draft,
        &[&role],
        involve,
    );
    let rows = cx.switch(&people, |_cx, people| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight);
        if people.is_empty() {
            column = column.push(Box::new(Text::new("tasks-project-no-people").dimmed()));
        }
        for (name, role) in people {
            let mut facts = vec![Fact::text(name.clone())];
            if !role.is_empty() {
                facts.push(Fact::text(role.clone()));
            }
            column = column.push(Box::new(Facts::new(facts)));
        }
        Box::new(column) as Element
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-project-people").with_role(TextRole::Heading),
            ))
            .push(rows)
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(
                        Container::new()
                            .with_flex_grow()
                            .with_no_padding()
                            .with_child(Box::new(name_field)),
                    ))
                    .push(Box::new(
                        Container::new()
                            .with_flex_grow()
                            .with_no_padding()
                            .with_child(Box::new(role_field)),
                    ))
                    .push(Box::new(button)),
            )),
    )
}

/// The tasks that work towards the project, at any depth under their
/// parent, each opening as a task does.
fn tasks(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let held = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .in_area(&id)
                .map(|(task, issue)| (task.clone(), issue.number, issue.title.clone()))
                .collect::<Vec<_>>()
        }
    });
    let rows = cx.switch(&held, move |cx, held| {
        if held.is_empty() {
            return Box::new(Text::new("tasks-project-no-tasks").dimmed()) as Element;
        }
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight);
        for (task, number, title) in held {
            let label = format!("{}  {title}", reference(*number));
            // Each task opens through the app's own published effect, the
            // one a script and an agent invoke: a project's list is not a
            // second way into an issue that only a click knows.
            column = column.push(Box::new(
                cx.effect_button(
                    label.as_str(),
                    &crate::TasksEffect::OpenIssue {
                        issue: task.clone(),
                    },
                )
                .role(ui::widget::ButtonRole::Plain),
            ));
        }
        Box::new(column)
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-project-tasks").with_role(TextRole::Heading),
            ))
            .push(rows),
    )
}

/// The documents the project holds: what its tasks link, so the project
/// shows them without storing a second copy.
fn documents(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let links = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            let mut links: Vec<_> = tracker
                .in_area(&id)
                .flat_map(|(_, issue)| issue.links.values().cloned())
                .collect();
            links.dedup_by(|left, right| left.resource == right.resource);
            links
        }
    });
    let chips = cx.switch(&links, |cx, links| {
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        if links.is_empty() {
            return Box::new(Text::new("tasks-project-no-documents").dimmed()) as Element;
        }
        for link in links {
            let reference = ui::tilo_reference::ResourceRef::whole(
                link.kind.clone(),
                link.resource.clone(),
                link.label.clone(),
            );
            row = row.push(Box::new(cx.reference(
                &link.app,
                link.record.as_deref(),
                &reference,
            )));
        }
        Box::new(row)
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-project-documents").with_role(TextRole::Heading),
            ))
            .push(chips),
    )
}

/// The one-line name a project is made with: the form the sidebar's button
/// opens.
///
/// The name is a text field like any other, and the form asks for the
/// tracker's own `create-area`, so making a project from the window and from
/// a script are the same act. It is made as a project; its page says whether
/// it goes on as an area, through that same kind verb.
pub(crate) struct NewProject {
    dialog: ui::widget::ModalDialog<Container>,
    asking: LocalState<bool>,
    shown: bool,
}

impl NewProject {
    pub(crate) fn new(
        cx: &ViewCx<Tracker>,
        asking: LocalState<bool>,
        name: &LocalState<String>,
    ) -> Self {
        let content = Container::new().with_no_padding().with_child(Box::new(
            TextInput::from_state(name)
                .with_id("tasks-new-project-name")
                .with_accessible_name(tr("tasks-field-project-name"))
                .with_placeholder("tasks-new-project-placeholder"),
        ));
        let create = {
            let (name, asking) = (name.clone(), asking.clone());
            let dispatcher = cx.dispatcher();
            move || {
                let Some(parsed) = NonBlank::parse(&name.get()) else {
                    return;
                };
                let created = tracker::CreateArea {
                    name: parsed,
                    kind: Some(Kind::Project),
                };
                asking.set(false);
                name.set(String::new());
                dispatcher.submit(Tracker::CREATE_AREA.with(created));
            }
        };
        let cancel = {
            let asking = asking.clone();
            move || asking.set(false)
        };
        let dismiss = {
            let asking = asking.clone();
            move || asking.set(false)
        };
        let dialog = ui::widget::ModalDialog::new(tr("tasks-new-project"), content)
            .with_test_id("tasks-new-project")
            .primary_action(tr("tasks-create-button"), create)
            .primary_enabled_if({
                let name = name.clone();
                move |_| NonBlank::parse(&name.get()).is_some()
            })
            .secondary_action(tr("tasks-cancel"), cancel)
            .on_dismiss(dismiss);
        Self {
            dialog,
            asking,
            shown: false,
        }
    }

    /// Whether it is showing, as the person left it.
    fn follow(&mut self) {
        let open = self.asking.get();
        if open != self.shown {
            self.dialog.set_open(open);
            self.shown = open;
        }
    }
}

impl ui::widget::Decorator for NewProject {
    type Inner = ui::widget::ModalDialog<Container>;

    fn inner(&self) -> &Self::Inner {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.dialog
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.asking.get();
        self.follow();
        ui::widget::Widget::update(&mut self.dialog, state, path) || asked != self.shown
    }

    fn name(&self) -> &'static str {
        "TasksNewProject"
    }
}
