//! Tasks in a hosted window: the palette and the form that make a task, the person's today, and
//! Today over Tasks' own document on one broker, its cards reading that document and what it changes
//! of it running at Tasks' api.

use super::*;

/// The node of the form's tree at `key`.
fn form_node<'a>(window: &'a Window, key: &str) -> Option<&'a patch::Node> {
    fn at<'a>(node: &'a patch::Node, key: &str) -> Option<&'a patch::Node> {
        if node.key == key {
            return Some(node);
        }
        node.children.iter().find_map(|child| at(child, key))
    }
    at(&window.form.as_ref()?.tree, key)
}

/// The choices a `choice` node offers, as the value a pick holds and the words it reads as.
fn choices_of(node: &patch::Node) -> Vec<(String, String)> {
    let Some(patch::Value::List(cases)) = node.props.get("cases") else {
        panic!("a choice with cases: {node:?}");
    };
    cases
        .iter()
        .map(|case| {
            let patch::Value::List(parts) = case else {
                panic!("a case: {case:?}");
            };
            match parts.as_slice() {
                [patch::Value::Text(value), patch::Value::Text(label), ..] => {
                    (value.clone(), label.clone())
                }
                other => panic!("a case's value and label: {other:?}"),
            }
        })
        .collect()
}

/// The palette runs "New task", which asks for nothing: a task needs no title to be made, so no
/// form stands for it and the press makes one there and then. Returns the task the press made.
fn a_task_added_from_the_palette(host: &mut TilolangProcess, window: &mut Window) -> Value {
    let new_task = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New task")
        .expect("the tasks publish the action that makes one")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: new_task,
        argument: None,
    }]);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "every argument it takes has a default, so nothing is asked: {:?}",
        window.form.as_ref().map(|form| form.title.clone())
    );
    let task = tasks_of(host.instances().next().expect("the instance runs"))
        .first()
        .cloned()
        .expect("the press made a task");
    let Value::Ref(made) = &task else {
        panic!("a task is a resource: {task:?}")
    };
    // The person has not said what it does yet, and the task is still named: a name every surface
    // that names it shows, so it is not blank in a window, a menu or a screen reader.
    let named = host
        .instances()
        .next()
        .expect("the instance runs")
        .instance()
        .host()
        .expect("the runtime")
        .runtime()
        .title(&made.target)
        .expect("the task is named");
    assert_eq!(
        named, "Task",
        "an untitled task is named by what it is until the person says what it does"
    );
    task
}

/// "Remind me…" asks when, the moments a person names offered to pick from, and one way into the
/// rest: the entry they type a moment into with the calendar at its end.
fn a_reminder_asks_when(host: &mut TilolangProcess, window: &mut Window) {
    let remind = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "Remind me…")
        .expect("the task offers its reminder")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: remind,
        argument: None,
    }]);
    settle(host, window);
    assert_eq!(
        window.form.as_ref().map(|form| form.title.as_str()),
        Some("Remind me…"),
        "the reminder's form asks when"
    );
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(""),
        "it starts with no moment, the task having no reminder"
    );
    assert!(
        !window.form.as_ref().expect("the form").ready,
        "a moment is required: nothing reads yet, so submit waits"
    );
    assert!(
        !form_node(window, "form~when~field")
            .expect("the moment's field")
            .props
            .contains_key("label"),
        "a form of one question says what it asks in its title"
    );
    let choices = choices_of(form_node(window, "form~when~quick").expect("the moments offered"));
    assert!(
        choices.iter().all(|(_, label)| label != "None"),
        "a required moment offers no way to leave it unanswered: {choices:?}"
    );
    assert_eq!(
        form_node(window, "form~when~quick")
            .expect("the moments offered")
            .props
            .get("case:as"),
        Some(&patch::Value::Text("time".to_owned())),
        "every moment says when it is, at the choice's trailing edge"
    );
    let picker = form_node(window, "form~when~pick").expect("one way into the rest");
    assert_eq!(
        picker
            .children
            .iter()
            .map(|child| (child.kind.as_str(), child.key.as_str()))
            .collect::<Vec<_>>(),
        [
            ("ui.TextInput", "form~when~text"),
            ("ui.DateField", "form~when"),
        ],
        "a moment is typed into the entry, or picked from the calendar at its end"
    );
    assert_eq!(
        form_node(window, "form~when")
            .expect("the calendar")
            .props
            .get("compact"),
        Some(&patch::Value::Bool(true)),
        "the calendar is a chip beside the entry, not a field of its own"
    );
    assert!(
        !window.form.as_ref().expect("the form").confirm,
        "the moments answer the form on one pick: its footer stands as Cancel alone"
    );
    assert_eq!(
        window.form.as_ref().expect("the form").answers,
        [form_node(window, "form~when~quick")
            .expect("its choices")
            .key
            .clone()],
        "picking a moment answers the form by itself"
    );
}

/// The person picks a moment from the ones the form offers, and that pick answers the form: the
/// reminder is set without a second press.
fn the_moment_picked_answers_the_reminder(
    host: &mut TilolangProcess,
    window: &mut Window,
    task: &Value,
) {
    let choices = choices_of(form_node(window, "form~when~quick").expect("the moments offered"));
    let (moment, _) = choices
        .iter()
        .find(|(_, label)| label == "Tomorrow morning")
        .unwrap_or_else(|| panic!("tomorrow morning is offered: {choices:?}"));
    type_into(window, "form~when~quick", moment);
    settle(host, window);
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(moment.as_str()),
        "the field reads as the moment the pick names"
    );
    submit_form(window);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "the form closes once the action runs: {:?}",
        window.form.as_ref().map(|form| form.error.clone())
    );
    assert_eq!(
        &reminder_of(host.instances().next().expect("the instance runs"), task),
        moment,
        "the task is reminded at the moment picked"
    );
}

tilo_test_env::scenario! {
    fn tasks_in_a_hosted_window_are_made_found_and_offered_at_the_api;

    phase the_palette_makes_a_task_and_asks_the_form_for_a_moment;
    // The palette runs an action whose arguments it does not give: "New task" takes a title whose
    // default is empty, so the task is made on the press and reads as its type names an untitled one,
    // and "Remind me…" asks its moment in a form, the person picking it in one press
    // (forms-and-diagrams.md §1).
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        assert_eq!(app, TASKS);
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
        settle(&mut host, &mut window);
        let task = a_task_added_from_the_palette(&mut host, &mut window);
        a_reminder_asks_when(&mut host, &mut window);
        the_moment_picked_answers_the_reminder(&mut host, &mut window, &task);
    }

    phase a_task_no_window_shows_opens_from_search_through_its_document;
    // A task's entry in the session's search index opens it in a window that shows none of its
    // items: the window stands on Today, so the Inbox task is published by no window, and the entry
    // names the `open` of the document that holds it, which every window showing the document publishes
    // and which a person choosing it in the palette runs as it is. Told the task, it takes the window
    // to the place that holds it — the Inbox, for one with no day — and opens the task there.
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", &app, bundle);
        settle(&mut host, &mut window);
        add_to_inbox(&mut host, &mut window, "Book the ferry");
        let task = tasks_of(host.instances().next().expect("the instance runs"))
            .first()
            .cloned()
            .expect("the entry made a task");
        let Value::Ref(made) = &task else {
            panic!("a task is a resource: {task:?}")
        };
        choose_place(&mut host, &mut window, "Today");
        assert_eq!(
            shown_place(&window).as_deref(),
            Some("Today"),
            "the window stands on Today"
        );
        let hosted = host.instances().next().expect("the instance runs");
        let published = tilolang_tilo::instance::wire::resource(&made.target).id;
        let path = published
            .strip_prefix(&format!("tl:{app}:"))
            .unwrap_or(&published);
        let entry = tilolang_tilo::search::index_entries_at(
            &hosted.instance().host().expect("the runtime"),
            &hosted.bundle().meta,
            path,
        )
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the task is found by its name: {published}"));
        let open = entry.action.expect("a hit opens what it names");
        let published_open = |resource: &str| {
            hosted
                .published()
                .iter()
                .find(|action| action.address.resource == resource && action.address.action == "open")
                .cloned()
        };
        assert!(
            published_open(&entry.resource).is_none(),
            "the window on Today publishes nothing of the Inbox task"
        );
        let record = published_open(&open.resource).unwrap_or_else(|| {
            panic!(
                "the hit names an `open` the window publishes: {}",
                open.resource
            )
        });
        assert_eq!(
            record.prompt_param(),
            None,
            "chosen as it is, the document's `open` asks for nothing"
        );
        let arguments = serde_json::json!({ "anchor": entry.resource }).to_string();
        inbox.borrow_mut().push_back(CollabEvent::Invocation {
            id: 42,
            invocation: tilo_actions::InvocationId::new(),
            resource: open.resource,
            action: open.action,
            argument: Some(format!(
                "{}{arguments}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            )),
            actor: tilo_actions::MemberId::person("ada").into(),
            context: None,
        });
        settle(&mut host, &mut window);
        assert!(
            matches!(
                answered(&sent, 42),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ),
            "{:?}",
            answered(&sent, 42)
        );
        assert_eq!(
            shown_place(&window).as_deref(),
            Some("Inbox"),
            "the task has no day: the window shows it in the Inbox"
        );
        assert_eq!(
            window.in_place.as_ref().map(|shown| shown.id.as_str()),
            Some(path),
            "the window shows the task as its page"
        );
    }

    phase a_fresh_tasks_offers_its_lists_actions_at_the_api_by_name;
    // An agent at the API of a Tasks no one has written in yet finds the list's actions by name:
    // `new_task` on the list, which the directory publishes at the app's top level.
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        let directory = Rc::new(RefCell::new(Directory::default()));
        let (mut host, mut window, _sent, _inbox, _route) =
            running_with("ada", &app, bundle, Rc::clone(&directory));
        settle(&mut host, &mut window);
        let found = page(
            &mut host,
            &mut window,
            &directory,
            ActionDirectoryQuery {
                needle: "new task".into(),
                ..query()
            },
        );
        let list = found
            .rows
            .iter()
            .find(|row| row.action.as_deref() == Some("new_task"))
            .unwrap_or_else(|| panic!("new_task is offered by name: {:?}", found.rows));
        assert!(
            list.resource.starts_with(&format!("tl:{app}:root-")),
            "{list:?}"
        );
        let top = page(&mut host, &mut window, &directory, query());
        assert!(
            top.rows.iter().any(|row| row.resource == list.resource),
            "the list is at the app's top level: {:?}",
            top.rows
        );
    }
}

/// The place the window's shell heads what it shows with: its overview's title, falling back to
/// its title. A shell whose content column holds an opened task gives that column the task's own
/// name and keeps the place holding it in `overview_title`.
fn shown_place(window: &Window) -> Option<String> {
    fn shell(node: &patch::Node) -> Option<&patch::Node> {
        if node.kind.ends_with("NavShell") {
            return Some(node);
        }
        node.children.iter().find_map(shell)
    }
    let shell = shell(window.root.as_ref()?)?;
    match shell
        .props
        .get("overview_title")
        .or_else(|| shell.props.get("title"))?
    {
        patch::Value::Text(title) => Some(title.clone()),
        _ => None,
    }
}

/// Choose the place `label` in the window's sidebar, as a person does.
fn choose_place(host: &mut TilolangProcess, window: &mut Window, label: &str) {
    let place = node_with(
        window.root.as_ref().expect("the window shows"),
        "label",
        label,
    )
    .unwrap_or_else(|| panic!("the window lists {label}"))
    .clone();
    let pick = place
        .events
        .keys()
        .next()
        .expect("a place is chosen")
        .clone();
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key: place.key,
        event: pick,
        value: None,
        gesture: None,
    }]);
    settle(host, window);
}

/// Type `title` into the Inbox's entry, as a person adds a task with no day.
fn add_to_inbox(host: &mut TilolangProcess, window: &mut Window, title: &str) {
    choose_place(host, window, "Inbox");
    let entry = node_with(
        window.root.as_ref().expect("the window shows"),
        "placeholder",
        "New task",
    )
    .expect("the Inbox's entry stands at the list's foot")
    .clone();
    let submit = entry
        .events
        .get_key_value("submit")
        .expect("the entry submits")
        .0
        .clone();
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key: entry.key,
        event: submit,
        value: Some(patch::Value::text(title)),
        gesture: None,
    }]);
    settle(host, window);
}

/// The day the task `task` is due, as its document holds it.
fn due_of(hosted: &Hosted, task: &Value) -> Option<String> {
    let Value::Ref(task) = task else {
        panic!("a task is a resource: {task:?}")
    };
    let instance = hosted.instance();
    let host = instance.host().expect("the runtime");
    match host.runtime().read(&task.target, "due").expect("its day") {
        Value::Opt(Some(day)) => match *day {
            Value::Text(day) => Some(day),
            other => panic!("a day: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("a day: {other:?}"),
    }
}

/// A task typed into Today's entry is due the person's today, and so stands under the day their
/// window heads Today: nine in the evening where they are is already the next day in UTC, and
/// their day, not UTC's, is the one their list is of.
#[test]
fn a_task_typed_into_todays_entry_is_due_the_persons_today() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_task_typed_into_todays_entry_is_due_the_persons_today",
        || {
            // 11 November, nine in the evening in Los Angeles: the 12th in UTC.
            let mut zone = tilo_test_env::ScopedEnv::new();
            zone.set("TZ", "America/Los_Angeles");
            let _clock = tilo_clock::testing::hold_wall_everywhere(
                tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process(),
                tilo_clock::time::UNIX_EPOCH + tilo_clock::time::Duration::from_secs(1_794_459_600),
            );
            let (app, bundle) = shipped(&shipped_dir().join("tasks"));
            assert_eq!(app, TASKS);
            let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
            settle(&mut host, &mut window);
            let entry = node_with(
                window.root.as_ref().expect("the window shows"),
                "placeholder",
                "New task for today",
            )
            .expect("Today's entry stands at the list's foot")
            .clone();
            let event = entry
                .events
                .get_key_value("submit")
                .expect("the entry submits what is typed into it")
                .0
                .clone();
            window.send(&[ToApp::Fire {
                level: Level::Root,
                key: entry.key,
                event,
                value: Some(patch::Value::text("File the report")),
                gesture: None,
            }]);
            settle(&mut host, &mut window);
            let held = host.instances().next().expect("the instance runs");
            let tasks = tasks_of(held);
            let [task] = tasks.as_slice() else {
                panic!("the entry made one task: {tasks:?}");
            };
            let due = due_of(held, task);
            assert_eq!(
                due.as_deref(),
                Some("2026-11-11"),
                "a task typed in their evening is due on their today, not UTC's tomorrow"
            );
            // The day a row is grouped under is the day the window heads by its own clock
            // (`tilo_ui_base::clock::today()`): the day it is due. They are one day, so the row heads
            // Today and stands in Today's list.
            // The zone the platform names, resolved against the database built into Tilo: the
            // window reads its day by it, not by whatever zoneinfo the machine installed.
            let zone = tilo_clock::local_zone_name()
                .and_then(|name| tilo_recurrence::zone::named(&name))
                .unwrap_or(jiff::tz::TimeZone::UTC);
            let headed_by = jiff::Timestamp::try_from(tilo_clock::to_std(tilo_clock::wall()))
                .unwrap()
                .to_zoned(zone)
                .date()
                .to_string();
            assert_eq!(
                due.as_deref(),
                Some(headed_by.as_str()),
                "and it stands under the day their window heads Today"
            );
        },
    );
}

/// Today and Tasks on one workspace, each over its own route, as their windows run in a session.
struct Shell {
    host: TilolangProcess,
    tasks: Window,
    today: Window,
    tasks_inbox: Inbox,
    /// What Tasks sent its broker: how each invocation it ran ended, which the dispatch answers
    /// the call's invoker with.
    tasks_sent: Sent,
    today_sent: Sent,
    today_inbox: Inbox,
    bus: Rc<Bus>,
    /// What Tasks last wrote of the workspace, as its store says.
    written: Option<String>,
    /// How many of Today's requests the broker took.
    taken: usize,
    /// How many of Today's requests the dispatch took, a press of the person's among them.
    dispatched: usize,
    /// How many of Tasks' answers the dispatch carried to their caller.
    answered: usize,
    /// The invocations the broker delivered.
    delivered: u64,
}

impl Shell {
    fn new() -> Self {
        let workspace = uuid::Uuid::new_v4();
        let bus = Rc::new(Bus::default());
        let mut host = TilolangProcess::new(wake());
        let tasks_route = RouteSpec::app(TASKS, workspace);
        let (tasks, tasks_sent, tasks_inbox) = instance_on(
            &mut host,
            spec(1, "ada", &tasks_route),
            shipped(&shipped_dir().join("tasks")).1,
            &tasks_route,
            Rc::default(),
            &bus,
        );
        // Notes, Tasks and Chat are installed as apps of their own; Discussion is not, and stays
        // part of the apps that use it.
        let installed = ["so.tilo.notes", TASKS, "so.tilo.chat"]
            .into_iter()
            .map(|app| (app.to_owned(), app.to_owned()))
            .collect();
        let today_route = RouteSpec::app("so.tilo.lang.today", workspace);
        let (today, today_sent, today_inbox) = instance_on(
            &mut host,
            Spec {
                installed,
                ..spec(2, "ada", &today_route)
            },
            shipped(&shipped_dir().join("today")).1,
            &today_route,
            Rc::default(),
            &bus,
        );
        Self {
            host,
            tasks,
            today,
            tasks_inbox,
            tasks_sent,
            today_sent,
            today_inbox,
            bus,
            written: None,
            taken: 0,
            dispatched: 0,
            answered: 0,
            delivered: 0,
        }
    }

    fn tasks_instance(&self) -> &Hosted {
        self.host
            .instances()
            .find(|hosted| hosted.spec().app == TASKS)
            .unwrap()
    }

    /// Run the host until it settles, the broker carrying between the two: what Tasks wrote of
    /// the workspace to Today, which reads it from the store, and what Today asks of Tasks' api to
    /// Tasks.
    fn settle(&mut self) {
        let began = std::time::Instant::now();
        loop {
            let mut changed = self.host.turn();
            self.tasks.take();
            self.today.take();
            changed |= self.carry_workspace();
            changed |= self.carry_presses();
            changed |= self.carry_asks();
            changed |= self.carry_answers();
            let woken = WOKEN.swap(false, std::sync::atomic::Ordering::SeqCst);
            if !changed && !woken && !self.host.owes_work() {
                return;
            }
            assert!(
                began.elapsed() < SETTLE_LIMIT,
                "the host owes work after {}s: {} opening, {} running",
                began.elapsed().as_secs(),
                self.host.opening(),
                self.host.instances().count()
            );
            if !changed {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// Tell Today the workspace's records moved, as the broker tells a connection when what
    /// Tasks wrote reached the store. Whether Tasks wrote anything since the last call.
    fn carry_workspace(&mut self) -> bool {
        let written = self.tasks_instance().store().last_written();
        if written == self.written {
            return false;
        }
        self.written = written;
        self.today_inbox
            .borrow_mut()
            .push_back(CollabEvent::RecordsMoved {
                records: self
                    .bus
                    .workspace()
                    .documents()
                    .into_iter()
                    .map(collab_types::store::MovedRecord::whole)
                    .collect(),
            });
        true
    }

    /// Hand Today's presses back to it, as the dispatch does once it has taken them: the calls a
    /// press made wait until the dispatch knows the press they continue, and go with this.
    /// Whether there were any.
    fn carry_presses(&mut self) -> bool {
        let presses: Vec<tilo_actions::remote::InvokeRequest> = self
            .today_sent
            .borrow()
            .get(self.dispatched..)
            .unwrap_or_default()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
                    ..
                } if invoke.target.provider == tilo_actions::remote::OWN_PROVIDER => {
                    Some(invoke.clone())
                }
                _ => None,
            })
            .collect();
        self.dispatched = self.today_sent.borrow().len();
        for press in &presses {
            self.delivered = self.delivered.saturating_add(1);
            self.today_inbox
                .borrow_mut()
                .push_back(CollabEvent::Invocation {
                    id: self.delivered,
                    invocation: press.invocation,
                    resource: press.target.address.resource.clone(),
                    action: press.target.address.action.clone(),
                    argument: press.argument.to_slot(),
                    actor: tilo_actions::MemberId::person("ada").into(),
                    context: None,
                });
        }
        !presses.is_empty()
    }

    /// Deliver to Tasks what Today asked of its API since the last call, as the person's
    /// invocation. Whether there was any.
    fn carry_asks(&mut self) -> bool {
        let asks: Vec<tilo_actions::remote::InvokeRequest> = self
            .today_sent
            .borrow()
            .get(self.taken..)
            .unwrap_or_default()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
                    ..
                } if invoke.target.provider.ends_with(&format!(":{TASKS}")) => Some(invoke.clone()),
                _ => None,
            })
            .collect();
        self.taken = self.today_sent.borrow().len();
        for invoke in &asks {
            self.deliver(
                invoke.invocation,
                &invoke.target.address,
                invoke.argument.to_slot(),
            );
        }
        !asks.is_empty()
    }

    /// Carry the answers Tasks gave, as the dispatch carries an app's answer to the caller that
    /// waits on it: Today's call goes on, and the calls after it go out. Whether there were any.
    fn carry_answers(&mut self) -> bool {
        let updates: Vec<tilo_actions::InvocationUpdate> = self
            .tasks_sent
            .borrow()
            .get(self.answered..)
            .unwrap_or_default()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::UpdateInvocation { update, .. } => Some(update.clone()),
                _ => None,
            })
            .collect();
        self.answered = self.tasks_sent.borrow().len();
        for update in &updates {
            let invocation = update.invocation();
            self.today_inbox
                .borrow_mut()
                .push_back(CollabEvent::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Update {
                        invocation,
                        state: Some(tilo_actions::remote::InvocationState::new(
                            invocation,
                            update.clone(),
                        )),
                        correlation: None,
                    },
                });
        }
        !updates.is_empty()
    }

    /// Deliver the action `address` with `argument` to Tasks as the invocation `invocation`, by
    /// ada: a call of Today's continues that call, so its answer reaches Today.
    fn deliver(
        &mut self,
        invocation: tilo_actions::InvocationId,
        address: &tilo_actions::ActionAddress,
        argument: Option<String>,
    ) {
        self.delivered = self.delivered.saturating_add(1);
        self.tasks_inbox
            .borrow_mut()
            .push_back(CollabEvent::Invocation {
                id: self.delivered,
                invocation,
                resource: address.resource.clone(),
                action: address.action.clone(),
                argument,
                actor: tilo_actions::MemberId::person("ada").into(),
                context: None,
            });
    }

    /// The address of the action Tasks publishes under `label` on a resource `pick` takes.
    fn tasks_action(
        &self,
        label: &str,
        pick: impl Fn(&str) -> bool,
    ) -> tilo_actions::ActionAddress {
        self.tasks_instance()
            .published()
            .iter()
            .find(|action| action.action_label == label && pick(&action.address.resource))
            .unwrap_or_else(|| panic!("Tasks publishes {label}"))
            .address
            .clone()
    }

    fn today_shows(&self, text: &str) -> bool {
        format!("{:?}", self.today.root).contains(text)
    }

    /// The titles of the tasks Tasks' resources hold.
    fn titles_in_tasks(&self) -> Vec<String> {
        let host = self.tasks_instance().instance().host().unwrap();
        let root = host.root_of(TASKS).unwrap();
        let tasks = host.package(TASKS).unwrap().meta;
        let items = tasks
            .types
            .iter()
            .find(|ty| ty.name == "Tasks")
            .and_then(|ty| ty.fields.iter().find(|field| field.name == "items"))
            .unwrap()
            .id
            .clone();
        let title = tasks
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .and_then(|ty| ty.fields.iter().find(|field| field.name == "title"))
            .unwrap()
            .id
            .clone();
        let Value::List(items) = host.runtime().read(&root, &items).unwrap() else {
            panic!("the tasks are a list");
        };
        items
            .iter()
            .filter_map(|item| match item {
                Value::Ref(task) => match host.runtime().read(&task.target, &title) {
                    Ok(Value::Text(title)) => Some(title),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

/// Today's cards read Tasks' resources from the workspace's store: a task due today that Tasks
/// makes shows in Today, one checked off in Tasks leaves it, and Today's Morning cleanup clears
/// the finished tasks at Tasks' api, in Tasks' resources.
#[test]
fn today_reads_tasks_from_the_workspace_and_clears_it_at_its_api() {
    let mut shell = Shell::new();
    shell.settle();

    let new_task = shell.tasks_action("New task", |resource| !resource.contains('/'));
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), "Water the plants".into());
    // Due today, the person's: what Today's place holds.
    let today = jiff::Timestamp::try_from(tilo_clock::to_std(tilo_clock::wall()))
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system())
        .date()
        .to_string();
    arguments.insert("due".to_owned(), today.into());
    shell.deliver(
        tilo_actions::InvocationId::new(),
        &new_task,
        tilo_actions::remote::InvokeArgument::Json(arguments).to_slot(),
    );
    shell.settle();
    assert_eq!(shell.titles_in_tasks(), ["Water the plants"]);
    assert!(
        shell.today_shows("Water the plants"),
        "the task Tasks made shows in Today: {:?}",
        shell.today.root
    );

    let complete = shell.tasks_action("Complete", |resource| resource.contains('/'));
    shell.deliver(tilo_actions::InvocationId::new(), &complete, None);
    shell.settle();
    assert!(
        !shell.today_shows("Water the plants"),
        "a task checked off in Tasks leaves Today's place"
    );

    let button = node_with(
        shell.today.root.as_ref().expect("Today is shown"),
        "label",
        "Morning cleanup",
    )
    .expect("Today offers its morning cleanup")
    .clone();
    let event = button
        .events
        .keys()
        .next()
        .expect("the button fires")
        .clone();
    shell.today.send(&[ToApp::Fire {
        level: Level::Root,
        key: button.key,
        event,
        value: None,
        gesture: None,
    }]);
    shell.settle();
    assert!(
        shell.titles_in_tasks().is_empty(),
        "the finished task went from Tasks' resources"
    );
    let filed = shell.today_sent.borrow().iter().any(|request| {
        matches!(request, CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
            ..
        } if invoke.target.provider.ends_with(":so.tilo.notes")
            && format!("{:?}", invoke.argument).contains("Morning: 1 done"))
    });
    assert!(filed, "the morning's note is asked of Notes' api");
}
