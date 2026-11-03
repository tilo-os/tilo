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

/// What the place a task's row stands in already says, and the row therefore leaves out.
enum RowPlace {
    /// Nothing: a plain list, a card, a surface of the system's own.
    full
    /// The day it is grouped under, as a list of a day is.
    day
    /// Its project, as a project's list is of one.
    project
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
    notes: RichText (body)
    /// Who is doing it.
    assignee: Option<Principal> = .none
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    /// The section of its project it is in, a lane of the project's board; none for the
    /// project's first lane.
    section: Option<Section> = .none
    priority: Priority = .none
    /// The task it is a subtask of; none for a task of its own. Tasks nest to any depth.
    parent: Option<Task> = .none

    search {
        filter "Inbox" = project == .none (icon: .inbox)
        filter "Today" = due <= today and not done (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter done "Done" (icon: .circle_check)
        filter due
        filter project
        filter priority
        hide at
        hide section
        hide parent
    }
}

/// One step of a task's checklist, from before a task's steps were tasks of their own.
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
    notes: RichText (body)
    /// Whether its window shows its tasks as a board, a lane per section, or as a list.
    board: Bool = false
}

/// A section of a project: one lane of its board, such as "Doing".
type Section {
    name: Text (title)
    /// The project it is a section of.
    project: Project
}

/// The workspace's tasks and projects.
type Tasks {
    /// Every task, in the order people put them.
    items: Collection<Task>
    projects: Collection<Project>
    /// The steps of every task, before a task's steps were tasks of their own.
    steps: Collection<Step> (previously)
    /// The sections of every project, each naming its project, in the order its board shows
    /// them.
    sections: Collection<Section>
}

root tasks: Tasks

default view home for Tasks

default view task_page for Task

default view project_page for Project

row view task_summary for Task

/// Gives each step of a task from before steps were tasks a task of its own, under the task it
/// was a step of, in the order it was in. A step whose task was removed becomes a task of its
/// own, keeping what it said: the step was reachable before, through nothing but its checklist.
migration action adopt_steps(tasks: Tasks) {
    for s in tasks.steps {
        // The task it was a step of, read only when it is still there: a removed task's fields
        // cannot be read.
        if let own = tasks.items.find(x -> x == s.task) {
            let made = tasks.items.insert(Task(title: s.title, done: s.done, parent: .some(own), project: own.project, section: own.section))
            let siblings = tasks.items.filter(x -> x.parent == .some(own) and x != made)
            if let last = siblings.last() { tasks.items.move(made, after: last) } else { tasks.items.move(made, after: own) }
        } else {
            tasks.items.insert(Task(title: s.title, done: s.done))
        }
    }
}

/// Adds what was typed in a quick add entry as a task, to the inbox or to `p`. When it says
/// when ("Call the landlord tomorrow 9am"), that is its due day and moment and the rest its
/// title; without, a task added `today` is due today. An empty entry adds nothing. It is the
/// entry's, not a command: the entry is how a task is added.
internal action quick_add(tasks: Tasks, entry: Text, p: Option<Project> = .none, today: Bool = false) {
    let said = dated(entry, now)
    let due: Option<Date> = if let day = said.day { .some(day) } else if today { dated("today", now).day } else { .none }
    if said.text != "" {
        tasks.items.insert(Task(title: said.text, due: due, at: said.at, project: p))
    }
}

/// Starts a task in the inbox: one to type into, or one under the title a program or an agent
/// gives it. The entry is how a person says what the task is.
primary action new_task(tasks: Tasks, title: Text = "") -> Task (icon: .plus, label: "New task", key: "Ctrl+N") {
    tasks.items.insert(Task(title: title))
}

/// Starts a project.
action new_project(tasks: Tasks) -> Project (icon: .folder_plus, label: "New project") {
    tasks.projects.insert(Project(name: "New project"))
}

/// Checks a task off, or opens it again.
action check(t: Task, done: Bool = not t.done) (label: if done { "Complete" } else { "Reopen" }, icon: .check, on: t.done, key: "Ctrl+Enter") {
    t.done = done
}

/// Takes a task on: it is the invoker's to do.
action take(t: Task) (label: "Assign to me", icon: .user) {
    require t.assignee == .none "Someone is doing it already"
    t.assignee = invoker
}

/// Gives a task to someone in the workspace to do.
action assign(t: Task, to: Principal) (label: "Assign to…", icon: .user) {
    t.assignee = to
}

/// Puts a task off until a day: it is due then.
action snooze(t: Task, until: Date) (label: "Snooze until…", icon: .clock) {
    require not t.done "It is done already"
    t.due = until
}

/// Leaves a task for anyone to take.
action release(t: Task) (label: "Unassign", icon: .user) {
    require t.assignee != .none "Nobody is doing it"
    t.assignee = .none
}

/// Files a task under a project.
action file(t: Task, p: Project) (label: "Move to {p.name}", icon: .folder, key: "Ctrl+Shift+M") {
    t.project = p
}

/// Takes a task out of its project, back to the inbox.
action unfile(t: Task) (label: "Move to inbox", icon: .inbox) {
    require t.project != .none "It is in the inbox already"
    t.project = .none
}

/// Sets the day a task is due by, or clears it.
action schedule(t: Task, due: Option<Date>) (label: "Set due date", icon: .calendar, key: "Ctrl+D") {
    t.due = due
}

/// Sets how much a task matters.
action prioritize(t: Task, priority: Priority) (icon: .flag) {
    t.priority = priority
}

/// Adds a subtask under a task: a task of its own, in its project and section. The subtasks
/// entry is how one is added.
internal action add_subtask(tasks: Tasks, t: Task, title: Text) {
    if title != "" {
        tasks.items.insert(Task(title: title, parent: .some(t), project: t.project, section: t.section))
    }
}

/// Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level
/// for none), where `before` stands when it is given, or at the end of the top level. A task
/// cannot go inside itself. The outline's drag, `Tab` and `Shift-Tab` name it; it is not a
/// command of its own.
internal action move_to(tasks: Tasks, moved: Task, parent: Option<Task>, before: Option<Task>) {
    require not within(tasks.items, parent, moved) and not within(tasks.items, before?.parent, moved) "A task cannot go inside itself"
    if let b = before {
        moved.parent = b.parent
        tasks.items.move(moved, before: b)
    } else {
        moved.parent = parent
        if let last = tasks.items.filter(x -> x.parent == parent and x != moved).last() { tasks.items.move(moved, after: last) } else if let p = parent { tasks.items.move(moved, after: p) }
    }
}

/// Moves a task under the task just over it, as Tab does in an outline.
action indent(t: Task) (label: "Indent", icon: .arrow_right) {
    let siblings = tasks.items.filter(x -> x.parent == t.parent)
    if let at = siblings.index_of(t) {
        if at > 0 {
            if let above = siblings.take(at).last() { move_to(tasks, t, .some(above), .none) }
        }
    }
}

/// Moves a task out from under its parent, just after it, as Shift-Tab does in an outline.
action outdent(t: Task) (label: "Outdent", icon: .arrow_left) {
    if let p = t.parent {
        t.parent = p.parent
        tasks.items.move(t, after: p)
    }
}

/// Deletes a task and its subtasks.
destructive action remove(tasks: Tasks, t: Task) (icon: .trash, key: "Delete") {
    for x in tasks.items.filter(x -> x == t or under(tasks.items, x, t)) {
        tasks.items.remove(x)
    }
}

/// Deletes every finished task, and answers how many went: a task whose subtasks are still
/// undone stays, so clearing never takes unfinished work with it.
destructive action clear_done(tasks: Tasks) -> Int (icon: .circle_check, label: "Clear done") {
    require tasks.items.count(x -> x.done) > 0 "Nothing is done yet"
    // Offered while something is done: whether a done task is finished (no open steps under
    // it) is read once it runs, where a whole list may be walked.
    let gone = tasks.items.filter(x -> finished(tasks.items, x))
    for x in gone {
        tasks.items.remove(x)
    }
    gone.len()
}

/// Deletes a project and its sections; its tasks go back to the inbox.
destructive action remove_project(tasks: Tasks, p: Project) (icon: .trash, label: "Delete project") {
    for t in tasks.items.filter(t -> t.project == p) {
        t.project = .none
        t.section = .none
    }
    tasks.sections.remove_where(s -> s.project == p)
    tasks.projects.remove(p)
}

/// Shows a project's tasks as a board, a lane per section, or as a list again. It is a switch a
/// project's window keeps beside its New task, not a command reached for occasionally.
action show_board(p: Project, on: Bool = not p.board) (label: "Board", icon: .layout_grid, on: p.board, frequent: true) {
    p.board = on
}

/// Adds a section at the end of a project's board.
action new_section(p: Project) -> Section (icon: .plus, label: "New section") {
    tasks.sections.insert(Section(name: "New section", project: p))
}

/// Renames a section.
action rename_section(s: Section, name: Text) {
    s.name = name
}

/// Deletes a section; its tasks go to the project's first lane.
destructive action remove_section(s: Section) (icon: .trash, label: "Delete section") {
    for t in tasks.items.filter(t -> t.section == s) {
        t.section = .none
    }
    tasks.sections.remove(s)
}

/// Adds a task at the end of a lane of a project's board: `s` names the lane, none for the
/// first. It is a project's primary action, so its window's New task adds to the first lane.
primary action add_to(p: Project, s: Option<Section> = .none) -> Task (icon: .plus, label: "New task", key: "Ctrl+N") {
    tasks.items.insert(Task(title: "", project: p, section: s))
}

/// Puts a task into a lane of its project's board, `s` (none for the first lane), at the place
/// `at` among the tasks already there. It is what a card's drag names.
internal action place(t: Task, s: Option<Section>, at: Int) {
    t.section = s
    tasks.items.move(t, to: at, among: tasks.items.filter(x -> x.project == t.project and x.section == s and not x.done))
}

/// Renames a project.
action rename(p: Project, name: Text) {
    p.name = name
}

/// Whether `top` is `x` or one of the tasks over it: the chain of parents, followed 64 deep —
/// deeper than anyone nests a task, so a chain that loops still ends.
fn under(items: List<Task>, x: Task, top: Task) -> Bool {
    let cur: Option<Task> = .some(x)
    let found = false
    for n in items.take(64) {
        if let at = cur {
            if at == top {
                found = true
            }
            cur = at.parent
        }
    }
    found
}

/// Whether every task under `t` is done too: a done task whose steps are unfinished is not
/// finished.
fn finished(items: List<Task>, t: Task) -> Bool {
    t.done and not items.any(x -> x != t and under(items, x, t) and not x.done)
}

/// Whether `place` is `top` or one of the tasks over it.
fn within(items: List<Task>, place: Option<Task>, top: Task) -> Bool {
    let here = false
    if let p = place {
        here = under(items, p, top)
    }
    here
}

/// The tasks of a project, in order.
fn tasks_of(p: Project) -> List<Task> {
    tasks.items.filter(t -> t.project == p)
}

/// The sections of a project, in order.
fn sections_of(p: Project) -> List<Section> {
    tasks.sections.filter(s -> s.project == p)
}

/// The open tasks of one lane of a project's board, in order: those of the section `s`, or of no
/// section. A subtask is not a card of its own; it goes where the task it is under goes.
fn lane_of(p: Project, s: Option<Section>) -> List<Task> {
    tasks.items.filter(t -> t.project == p and t.section == s and not t.done and t.parent == .none)
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
    let shown = t.items.matching(place).sort_by(.due)
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
                std.rows(shown, "No tasks here", .none, .some("list-checks"), group: .due, closed: .done) { x ->
                    task_row(x, .day)
                }
            }
        }
        content: {
            ui.EmptyState("No task selected", icon: .list_checks)
        }
    }
}

/// A project's window: its tasks, in order, with the entry that adds to it at their foot; or,
/// shown as a board (its Board toggle), the same tasks as a lane per section.
view project_page(p: Project) {
    lens draft: Text
    if p.board {
        project_board(p)
    } else {
        project_list(p, draft)
    }
}

/// A project's tasks as a list, with the entry that adds to it at their foot.
view project_list(p: Project, draft: State<Text>) {
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
                    task_row(x, .project)
                }
            }
        }
        content: {
            ui.EmptyState("No task selected", icon: .list_checks)
        }
    }
}

/// A project's board: a lane per section, the first for the tasks in none, each open task a
/// card. A card is dragged to another place in its lane or into another lane, or moved from
/// its menu, which also holds its actions, and opens in a window of its own.
view project_board(p: Project) {
    let sections = sections_of(p)
    std.page(p, canvas: true) {
        ui.Scroll(across: true) {
            ui.Board(name: p.name) {
                let loose = lane_of(p, .none)
                ui.BoardLane(title: "No section", count: loose.len(), action: "New task") {
                    on act: add_to(p, .none)
                    on drop(moved: Task, at: Int): place(moved, .none, at)
                    for t in loose {
                        task_card(t, sections)
                    }
                }
                for s in sections {
                    let cards = lane_of(p, .some(s))
                    ui.BoardLane(s, title: s.name, count: cards.len(), action: "New task") {
                        on act: add_to(p, .some(s))
                        on drop(moved: Task, at: Int): place(moved, .some(s), at)
                        for t in cards {
                            task_card(t, sections)
                        }
                    }
                }
            }
        }
    }
}

/// A task on a board: its row. It opens in a window of its own, and its menu moves it to
/// another lane or runs its actions.
view task_card(t: Task, sections: List<Section>) {
    ui.BoardCard(t) {
        on activate: open(t)
        menu: {
            if t.section != .none {
                ui.MenuItem("Move to No section", icon: .arrow_right) {
                    on choose: place(t, .none, 0)
                }
            }
            for s in sections {
                if t.section != .some(s) {
                    ui.MenuItem("Move to {s.name}", icon: .arrow_right) {
                        on choose: place(t, .some(s), 0)
                    }
                }
            }
            for a in actions(t) {
                ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                    on choose: a.run
                }
            }
        }
        task_row(t, .project)
    }
}

/// One task as a row: whether it is done, its title, its project, when it is due, a flag while
/// it matters, and who is doing it. `place` leaves out what the place the row stands in already
/// says — the day a list of a day is grouped by, the project a project's list is of — so a row
/// repeats nothing the page around it states.
view task_row(t: Task, place: RowPlace) {
    let project: Option<Text> = if place == .project { .none } else { t.project?.name }
    let due: Option<Date> = if place == .day { .none } else { t.due }
    row(t.title, checkbox: t.done, secondary: project, meta: due, mark: t.priority.icon(), avatar: t.assignee)
}

/// A task as the system shows it on a surface of its own — a result, a notice — where no page
/// around it says anything: its row with every fact.
view task_summary(t: Task) {
    row(t.title, checkbox: t.done, secondary: t.project?.name, meta: t.due, mark: t.priority.icon(), avatar: t.assignee)
}

/// One task in an outline, which counts its done subtasks while it is folded. `project` is the
/// project of the page around the outline: a row repeats no project the page already names, and
/// names its own while the subtask is of another.
view task_outline_row(t: Task, project: Option<Project>) {
    ui.TreeViewRow(checkbox: t.done, icon: t.priority.icon(), secondary: if t.project == project { .none } else { t.project?.name }, trailing: t.due)
}

/// A task's window: whether it is done and its title, when it is due, its project, how much it
/// matters, who is doing it, its subtasks, its notes, and what happened to it.
view task_page(t: Task) {
    lens step: Text
    std.document(t) {
        row(gap: .related, align: .center) {
            checkbox(bind: t.done)
            input(bind: t.title, placeholder: "Title", fill)
        }
        field("Due") {
            ui.DateField(bind: t.due, optional: true)
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
                row(gap: .related, align: .center) {
                    button(take(t))
                    button(assign(t))
                }
            }
        }
        section("Subtasks") {
            // A task with no subtasks puts no tree up, and a tree of one task's subtasks pays
            // for that walk only.
            let direct = tasks.items.filter(x -> x.parent == .some(t))
            if not direct.is_empty() {
                // Every task under this one, at any depth: the outline nests each under its
                // parent.
                let kids = tasks.items.filter(x -> x != t and under(tasks.items, x, t))
                std.outline("Subtasks", kids, x -> x.parent, x -> x.title, "No subtasks yet", (moved, parent, before) -> move_to(tasks, moved, parent, before), x -> check(x), fill: false) { x ->
                    task_outline_row(x, t.project)
                }
            }
            ui.TextInput(value: step, placeholder: "Add a subtask") {
                on submit: add_subtask(tasks, t, step).then(set(step, ""))
            }
        }
        ui.BlockEditor(bind: t.notes, placeholder: "Add notes")
        std.history(t)
    }
}

/// A household and a small team in the same week: an inbox to sort, two projects, some of it
/// due today, some of it done.
scenario "a busy week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(Project(name: "Move flat", notes: RichText("Keys on the 15th. The van is booked for the morning.")))
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    tasks.items.insert(Task(title: "Take the bins out", done: true))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    let call = tasks.items.insert(Task(title: "Call the landlord about the boiler", notes: RichText("Ask whether Tuesday morning works for the engineer."), due: 2026-06-30, at: 2026-06-30T09:00Z, priority: .high))
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Reply to Mia about Sunday lunch", due: 2026-07-01))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    tasks.items.insert(Task(title: "Give notice to the electricity company", project: flat, due: 2026-07-04, priority: .medium))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Pack the books", project: flat, done: true))
    let copy = tasks.items.insert(Task(title: "Write the launch post", project: launch, due: 2026-07-06, priority: .urgent, notes: RichText("Lead with the offline editor, then sharing.")))
    let outline = tasks.items.insert(Task(title: "Outline", done: true, parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Collect the beta quotes", parent: .some(outline), project: launch))
    tasks.items.insert(Task(title: "First draft", parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Screenshots", parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Check the pricing page on phones", project: launch, due: 2026-07-08))
    tasks.items.insert(Task(title: "Set up the redirects", project: launch, done: true))
    as mia at 2026-06-29T16:00Z {
        take(copy)
    }
    call
}

/// A launch post half written: its window, with the subtasks it was broken into, one of them
/// broken into its own.
scenario "a task's subtasks" -> Task {
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    let copy = tasks.items.insert(Task(title: "Write the launch post", project: launch, due: 2026-07-06, priority: .urgent, notes: RichText("Lead with the offline editor, then sharing.")))
    let outline = tasks.items.insert(Task(title: "Outline", done: true, parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Collect the quotes from the beta testers", parent: .some(outline), project: launch))
    tasks.items.insert(Task(title: "First draft, with the quotes from the beta testers", parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Screenshots", parent: .some(copy), project: launch))
    copy
}

/// A week everyone has finished.
scenario "all done" {
    tasks.items.insert(Task(title: "Passports", done: true))
    tasks.items.insert(Task(title: "Chargers", done: true))
    tasks.items.insert(Task(title: "Sun cream", done: true))
}

/// A move a week away, on its project's board: a lane per stage, the tasks in each.
scenario "a project board" -> Project {
    let flat = tasks.projects.insert(Project(name: "Move flat", notes: RichText("Keys on the 15th."), board: true))
    let packing = tasks.sections.insert(Section(name: "Packing", project: flat))
    let booked = tasks.sections.insert(Section(name: "Booked", project: flat))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Ask Mia about the sofa", project: flat, due: 2026-07-01))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, section: packing, due: 2026-07-02, priority: .medium))
    tasks.items.insert(Task(title: "Pack the books", project: flat, section: packing, done: true))
    tasks.items.insert(Task(title: "Pack the kitchen", project: flat, section: packing, due: 2026-07-10))
    tasks.items.insert(Task(title: "Book the van", project: flat, section: booked, priority: .high))
    tasks.items.insert(Task(title: "Give notice to the electricity company", project: flat, section: booked, due: 2026-07-04))
    flat
}
