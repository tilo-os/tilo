app so.tilo.lang.tasks "Tasks" (icon: .list_checks, category: .office, previously: so.tilo.lang.todo)

/// How much a task matters, most urgent last.
enum Priority {
    /// No priority
    none
    /// Low
    low (icon: .flag)
    /// Medium
    medium (icon: .flag)
    /// High
    high (icon: .flag)
    /// Urgent
    urgent (icon: .triangle_alert)
}

/// Something to do: what, by when, by whom, and in which project.
type Task {
    title: Text (title)
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// The moment on that day it is due at, if it has one.
    at: Option<Time> = .none
    /// Anything worth remembering about it.
    notes: Text (body)
    /// Who is doing it.
    assignee: Option<Principal> = .none
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    priority: Priority = .none

    search {
        filter "Inbox" = project == .none (icon: .inbox)
        filter "Today" = due <= today and not done (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter done "Done" (icon: .circle_check)
        filter due
        filter project
        filter priority
        hide at
    }
}

/// One step of a task: a line of its checklist.
type Step {
    title: Text (title)
    done: Bool = false
    /// The task it is a step of.
    task: Task
}

/// A project: tasks that work towards one outcome.
type Project {
    name: Text (title)
    /// What it is for, and anything worth remembering about it.
    notes: Text (body)
}

/// The workspace's tasks and projects.
type Tasks {
    /// Every task, in the order people put them.
    items: Collection<Task>
    projects: Collection<Project>
    /// The steps of every task, each naming its task: a task's checklist is the run of its
    /// steps, which the index answers without reading the others.
    steps: Collection<Step>
}

root tasks: Tasks

default view home for Tasks

default view task_page for Task

default view project_page for Project

row view task_row for Task

row view step_row for Step

/// Adds a task with this title to the inbox.
action add(tasks: Tasks, title: Text) -> Task {
    tasks.items.insert(Task(title: title))
}

/// Adds what was typed in a quick add entry as a task, to the inbox or to `p`. When it says
/// when ("Call the landlord tomorrow 9am"), that is its due day and moment and the rest its
/// title; without, a task added `today` is due today. An empty entry adds nothing.
action quick_add(tasks: Tasks, entry: Text, p: Option<Project> = .none, today: Bool = false) {
    let said = dated(entry, now)
    let due: Option<Date> = if let day = said.day {
        .some(day)
    } else if today {
        dated("today", now).day
    } else {
        .none
    }
    if said.text != "" {
        tasks.items.insert(Task(title: said.text, due: due, at: said.at, project: p))
    }
}

/// Starts a task in the inbox.
primary action new_task(tasks: Tasks) -> Task (icon: .plus, label: "New task") {
    tasks.items.insert(Task(title: ""))
}

/// Starts a project.
action new_project(tasks: Tasks) -> Project (icon: .folder_plus, label: "New project") {
    tasks.projects.insert(Project(name: "New project"))
}

/// Checks a task off, or opens it again.
action check(t: Task, done: Bool = not t.done) (label: if done { "Complete" } else { "Reopen" }, icon: .check, on: t.done) {
    t.done = done
}

/// Takes a task on: it is the invoker's to do.
action take(t: Task) (label: "Assign to me", icon: .user) {
    require t.assignee == .none "Someone is doing it already"
    t.assignee = invoker
}

/// Leaves a task for anyone to take.
action release(t: Task) (label: "Unassign", icon: .user) {
    require t.assignee != .none "Nobody is doing it"
    t.assignee = .none
}

/// Files a task under a project.
action file(t: Task, p: Project) (label: "Move to {p.name}", icon: .folder) {
    t.project = p
}

/// Takes a task out of its project, back to the inbox.
action unfile(t: Task) (label: "Move to inbox", icon: .inbox) {
    require t.project != .none "It is in the inbox already"
    t.project = .none
}

/// Sets the day a task is due by, or clears it.
action schedule(t: Task, due: Option<Date>) (label: "Set due date", icon: .calendar) {
    t.due = due
}

/// Sets how much a task matters.
action prioritize(t: Task, priority: Priority) (icon: .flag) {
    t.priority = priority
}

/// Adds a step to a task's checklist.
action add_step(tasks: Tasks, t: Task, title: Text) {
    if title != "" {
        tasks.steps.insert(Step(title: title, task: t))
    }
}

/// Checks a step off, or opens it again.
action check_step(s: Step, done: Bool = not s.done) (label: if done { "Complete" } else { "Reopen" }, icon: .check, on: s.done) {
    s.done = done
}

/// Removes a step from a task's checklist.
destructive action remove_step(tasks: Tasks, s: Step) (icon: .trash) {
    tasks.steps.remove(s)
}

/// Deletes a task.
destructive action remove(tasks: Tasks, t: Task) (icon: .trash) {
    tasks.items.remove(t)
}

/// Deletes every task that is done.
destructive action clear_done(tasks: Tasks) -> Int (icon: .circle_check, label: "Clear done") {
    require tasks.items.any(.done) "Nothing is done yet"
    tasks.items.remove_where(.done)
}

/// Deletes a project; its tasks go back to the inbox.
destructive action remove_project(tasks: Tasks, p: Project) (icon: .trash, label: "Delete project") {
    for t in tasks.items.filter(t -> t.project == p) {
        t.project = .none
    }
    tasks.projects.remove(p)
}

/// Moves a task to just after another.
action move(tasks: Tasks, t: Task, after: Task) {
    tasks.items.move(t, after: after)
}

/// Renames a project.
action rename(p: Project, name: Text) {
    p.name = name
}

/// The steps of a task, in order.
fn steps_of(t: Task) -> List<Step> {
    tasks.steps.filter(s -> s.task == t)
}

/// The tasks of a project, in order.
fn tasks_of(p: Project) -> List<Task> {
    tasks.items.filter(t -> t.project == p)
}

/// The tasks window: Inbox, Today, Upcoming and Done, each the tasks the filter of that name
/// holds of, and the projects, each opening in a window of its own. The place chosen is the
/// window's, so everyone looking sees the same list; what is typed in the entry at its foot is
/// each person's own until Enter adds it.
view home(t: Tasks) {
    window place: Text = "is:inbox"
    lens draft: Text
    // The named filters are the places; a priority is picked in search.
    let places = filters(Task).filter(f -> f.toggle and f.key != "priority")
    let shown = t.items.matching(place)
    let title = places.find(p -> p.query == place)?.label ?? "Tasks"
    ui.NavShell(content_kind: .document, sidebar_title: "Tasks", overview_title: title, overview_count: shown.len()) {
        sidebar: {
            ui.NavList(selected: place) {
                for p in places {
                    ui.NavItem(p.query, p.label, icon: p.icon, count: t.items.matching(p.query).len()) {
                        on select: set(place, p.query)
                    }
                }
                for p in t.projects {
                    ui.NavItem("project {p.name}", p.name, icon: .folder) {
                        on select: open(p)
                    }
                }
            }
        }
        overview_actions: {
            std.header(actions(t))
        }
        overview: {
            ui.Docked {
                foot: {
                    stack {
                        // On Today, what is added is due today unless it says when.
                        if place == "is:today" {
                            ui.TextInput(value: draft, placeholder: "New task for today") {
                                on submit: quick_add(t, draft, .none, true).then(set(draft, ""))
                            }
                        } else {
                            ui.TextInput(value: draft, placeholder: "New task") {
                                on submit: quick_add(t, draft).then(set(draft, ""))
                            }
                        }
                    }
                }
                std.rows(shown, "No tasks here", .none, .some("list-checks")) { x ->
                    task_row(x)
                }
            }
        }
        content: {
            ui.EmptyState("No task selected", icon: .list_checks)
        }
    }
}

/// A project's window: its tasks, in order, with the entry that adds to it at their foot.
view project_page(p: Project) {
    lens draft: Text
    let shown = tasks_of(p)
    ui.NavShell(content_kind: .document, overview_title: p.name, overview_count: shown.len()) {
        overview_actions: {
            std.header(actions(p))
        }
        overview: {
            ui.Docked {
                foot: {
                    stack {
                        ui.TextInput(value: draft, placeholder: "New task in {p.name}") {
                            on submit: quick_add(tasks, draft, p).then(set(draft, ""))
                        }
                    }
                }
                std.rows(shown, "No tasks yet", .none, .some("folder")) { x ->
                    task_row(x)
                }
            }
        }
        content: {
            ui.EmptyState("No task selected", icon: .list_checks)
        }
    }
}

/// One task in a list: a checkbox for whether it is done, its title, its project, when it is
/// due, a flag while it matters, and who is doing it.
view task_row(t: Task) {
    row(t.title, checkbox: t.done, secondary: t.project?.name, meta: t.due, mark: t.priority.icon(), avatar: t.assignee)
}

/// One step of a task's checklist: a checkbox for whether it is done, and what it is.
view step_row(s: Step) {
    row(s.title, checkbox: s.done)
}

/// A task's window: whether it is done and its title, when it is due, its project, how much it
/// matters, who is doing it, its steps and its notes.
view task_page(t: Task) {
    lens step: Text
    std.document(t) {
        row(gap: .related, align: .center) {
            checkbox(bind: t.done)
            input(bind: t.title, placeholder: "Title", fill)
        }
        field("Due") {
            date(bind: t.due)
        }
        field("Project") {
            ui.MenuButton(t.project?.name ?? "Inbox", icon: .folder) {
                ui.MenuItem("Inbox", icon: .inbox, checked: t.project == .none) {
                    on choose: unfile(t)
                }
                for p in tasks.projects {
                    ui.MenuItem(p.name, icon: .folder, checked: t.project == p) {
                        on choose: file(t, p)
                    }
                }
            }
        }
        field("Priority") {
            ui.MenuButton(t.priority.label(), icon: .flag) {
                for c in cases(Priority) {
                    ui.MenuItem(c.label(), checked: c == t.priority) {
                        on choose: prioritize(t, c)
                    }
                }
            }
        }
        field("Assignee") {
            if let who = t.assignee {
                row(gap: .related, align: .center) {
                    avatar(who)
                    button(release(t))
                }
            } else {
                button(take(t))
            }
        }
        section("Steps") {
            let steps = steps_of(t)
            if not steps.is_empty() {
                ui.VirtualList {
                    for s in steps {
                        ui.ListItem(item: s) {
                            step_row(s)
                        }
                    }
                }
            }
            ui.TextInput(value: step, placeholder: "Add a step") {
                on submit: add_step(tasks, t, step).then(set(step, ""))
            }
        }
        input(bind: t.notes, placeholder: "Add notes", lines: 6)
    }
}

/// A household and a small team in the same week: an inbox to sort, two projects, some of it
/// due today, some of it done.
scenario "a busy week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(Project(name: "Move flat", notes: "Keys on the 15th. The van is booked for the morning."))
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    tasks.items.insert(Task(title: "Take the bins out", done: true))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    let call = tasks.items.insert(Task(title: "Call the landlord about the boiler", notes: "Ask whether Tuesday morning works for the engineer.", due: 2026-06-30, at: 2026-06-30T09:00Z, priority: .high))
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Reply to Mia about Sunday lunch", due: 2026-07-01))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    tasks.items.insert(Task(title: "Give notice to the electricity company", project: flat, due: 2026-07-04, priority: .medium))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Pack the books", project: flat, done: true))
    let copy = tasks.items.insert(Task(title: "Write the launch post", project: launch, due: 2026-07-06, priority: .urgent, notes: "Lead with the offline editor, then sharing."))
    tasks.steps.insert(Step(title: "Outline", done: true, task: copy))
    tasks.steps.insert(Step(title: "First draft", task: copy))
    tasks.steps.insert(Step(title: "Screenshots", task: copy))
    tasks.items.insert(Task(title: "Check the pricing page on phones", project: launch, due: 2026-07-08))
    tasks.items.insert(Task(title: "Set up the redirects", project: launch, done: true))
    as mia at 2026-06-29T16:00Z {
        take(copy)
    }
    call
}

/// A team's project for a release a week out: what is left, and what is done.
scenario "a release list" {
    let r24 = tasks.projects.insert(Project(name: "Release 2.4"))
    tasks.items.insert(Task(title: "Freeze the strings for translation", project: r24, done: true, due: 2026-10-26))
    tasks.items.insert(Task(title: "Record the demo video", project: r24, due: 2026-10-30))
    tasks.items.insert(Task(title: "Update the changelog", project: r24, notes: "Group the fixes by app.", due: 2026-10-31))
    tasks.items.insert(Task(title: "Send the beta to the testers", project: r24, done: true))
    tasks.items.insert(Task(title: "Check the upgrade from 2.3", project: r24, due: 2026-11-01))
}

/// A week everyone has finished.
scenario "all done" {
    tasks.items.insert(Task(title: "Passports", done: true))
    tasks.items.insert(Task(title: "Chargers", done: true))
    tasks.items.insert(Task(title: "Sun cream", done: true))
}
