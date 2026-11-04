app so.tilo.lang.tasks "Tasks" (
    icon: .list_checks,
    category: .office,
    previously: so.tilo.lang.todo,
)

/// How much a task matters, most urgent last.
enum Priority {
    /// No priority
    none
    /// Low
    low
    /// Medium
    medium (icon: .flag, tone: .info)
    /// High
    high (icon: .flag, tone: .warning)
    /// Urgent
    urgent (icon: .triangle_alert, tone: .danger)
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
    done: Bool (done) = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// The moment on that day it is due at, if it has one.
    at: Option<Time> (time of: due) = .none
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
    parent: Option<Task> (parent) = .none
    /// Whether it is finished: done itself, and so is every task under it. A task with steps is
    /// finished when its steps are, so the branch two people finish at once is finished once
    /// both changes are in, though neither change named it.
    derive finished: Bool = children.all(finished) and (done or children.count > 0)
    /// Whether it stands in a list's Done tail: finished, and so is every task it is under, so
    /// a step done with under a task still open stays where it stands, checked and muted.
    derive folded: Bool = finished and parent.all(folded)
    /// How many tasks are under it, and how many of those are done ("1/3"): the two counts a
    /// row and the page's outline show.
    derive subtasks: Int = children.count
    derive subtasks_done: Int = children.count(done)

    search {
        filter "Inbox" = inbox(filed: project, day: due) (icon: .inbox)
        filter "Today" = agenda(day: due) (icon: .star)
        filter "Upcoming" = upcoming(day: due) (icon: .calendar)
        filter "Done" = logbook (icon: .circle_check)
        filter due
        filter project
        filter priority
        filter parent
        hide at
        hide section
        hide folded
        hide subtasks
        hide subtasks_done
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
            let made = tasks.items.insert(
                Task(
                    title: s.title,
                    done: s.done,
                    parent: .some(own),
                    project: own.project,
                    section: own.section,
                ),
            )
            let last = tasks.items.filter(x -> x.parent == .some(own) and x != made).last() ?? own
            tasks.items.move(made, after: last)
        } else {
            tasks.items.insert(Task(title: s.title, done: s.done))
        }
    }
}

/// Adds what was typed in the entry at the end of a list as a task, to the inbox or to `p`, in
/// the lane `s` of its board. When it says when ("Call the landlord tomorrow 9am"), that is its
/// due day and moment and the rest its title; without, a task added on Today is due today. An
/// entry with nothing in it adds nothing. It is the entry's, not a command: the entry is how a
/// task is added there, and the task joins the list in place rather than opening.
internal action quick_add(
    tasks: Tasks,
    entry: Text,
    p: Option<Project> = .none,
    today: Bool = false,
    s: Option<Section> = .none,
) {
    let said = dated(entry, now)
    let due: Option<Date> = if let day = said.day {
        .some(day)
    } else if today { dated("today", now).day } else { .none }
    if said.text != "" {
        tasks.items.insert(Task(title: said.text, due: due, at: said.at, project: p, section: s))
    }
}

/// Adds a task under the title a program or an agent gives it: no task stands without one. A
/// person adds one by typing it in the entry at the end of the list, which is what this
/// action's key and its menu row bring them to.
primary action new_task(tasks: Tasks, title: Text) -> Task (
    icon: .plus,
    label: "New task",
    key: "Ctrl+N",
) {
    tasks.items.insert(Task(title: title))
}

/// Starts a project.
action new_project(tasks: Tasks) -> Project (icon: .folder_plus, label: "New project") {
    tasks.projects.insert(Project(name: "New project"))
}

/// Checks a task off with every task under it, or opens it again with the tasks it is under.
action check(t: Task, done: Bool = not t.done) (
    label: if done { "Complete" } else { "Reopen" },
    icon: .check,
    on: t.done,
    key: "Ctrl+Enter",
) {
    t.done = done
    if done {
        for x in tasks.items.descendants(t, .parent) {
            x.done = true
        }
    } else {
        // Everything above the one opened is open again.
        for a in tasks.items.ancestors(t, .parent) {
            a.done = false
        }
    }
}

/// Adds a subtask under a task: a task of its own, in its project and section, under the title
/// given. It stands with its siblings, and a task that was done opens again: there is work under
/// it now.
action add_subtask(tasks: Tasks, t: Task, title: Text) -> Task (
    label: "Add subtask",
    icon: .list_tree,
    key: "Ctrl+Shift+N",
) {
    let made = tasks.items.insert(
        Task(title: title, parent: .some(t), project: t.project, section: t.section),
    )
    let last = tasks.items.filter(x -> x.parent == .some(t) and x != made).last() ?? t
    tasks.items.move(made, after: last)
    if t.done or tasks.items.ancestors(t, .parent).any(a -> a.done) { check(t, false) }
    made
}

/// Sets the day a task is due by, or clears it: the presets take the day, and one entry asks
/// for any day.
action due_today(t: Task) (label: "Today", icon: .star, key: "Ctrl+T", group: "Due") {
    require not t.done "It is done already"
    t.due = dated("today", now).day
}

/// Makes a task due tomorrow.
action due_tomorrow(t: Task) (label: "Tomorrow", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.due = dated("tomorrow", now).day
}

/// Makes a task due at the end of next week.
action due_next_week(t: Task) (label: "Next week", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.due = dated("next week", now).day
}

/// Makes a task due on a day the person picks.
action schedule(t: Task, due: Option<Date>) (
    label: "Pick date…",
    icon: .calendar,
    key: "Ctrl+D",
    group: "Due",
) {
    t.due = due
}

/// Clears the day a task is due by: it waits in the inbox, undated.
action due_clear(t: Task) (label: "No date", group: "Due") {
    require t.due != .none "It has no date"
    t.due = .none
}

/// Sets how much a task matters.
action set_priority_none(t: Task) (label: "No priority", icon: .flag, group: "Priority") {
    require t.priority != .none "It has no priority"
    t.priority = .none
}

/// Marks a task as low priority.
action set_priority_low(t: Task) (label: "Low", icon: .flag, group: "Priority") {
    require t.priority != .low "It is low already"
    t.priority = .low
}

/// Marks a task as medium priority.
action set_priority_medium(t: Task) (label: "Medium", icon: .flag, group: "Priority") {
    require t.priority != .medium "It is medium already"
    t.priority = .medium
}

/// Marks a task as high priority.
action set_priority_high(t: Task) (label: "High", icon: .flag, group: "Priority") {
    require t.priority != .high "It is high already"
    t.priority = .high
}

/// Marks a task as urgent.
action set_priority_urgent(t: Task) (label: "Urgent", icon: .triangle_alert, group: "Priority") {
    require t.priority != .urgent "It is urgent already"
    t.priority = .urgent
}

/// Files a task under a project the person picks.
action file(t: Task, p: Project) (
    label: "Project…",
    icon: .folder,
    key: "Ctrl+Shift+M",
    group: "Move to project",
) {
    t.project = p
}

/// Takes a task out of its project, back to the inbox.
action unfile(t: Task) (label: "Inbox", icon: .inbox, group: "Move to project") {
    require t.project != .none "It is in the inbox already"
    t.project = .none
}

/// Gives a task to someone, or to nobody.
action assign(t: Task, who: Option<Principal>) (label: "Assign…", icon: .user) {
    t.assignee = who
}

/// Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level
/// for none), where `before` stands when it is given, or at the end of the top level. A task
/// cannot go inside itself, and open work put under a done task opens it. The outline's drag,
/// `Ctrl+]` and `Ctrl+[` name it; it is not a command of its own.
internal action move_to(tasks: Tasks, moved: Task, parent: Option<Task>, before: Option<Task>) {
    require not within(tasks.items, parent, moved)
        and not within(tasks.items, before?.parent, moved) "A task cannot go inside itself"
    if let b = before {
        moved.parent = b.parent
        tasks.items.move(moved, before: b)
    } else {
        moved.parent = parent
        if let last = tasks.items.filter(x -> x.parent == parent and x != moved).last() {
            tasks.items.move(moved, after: last)
        } else if let p = parent { tasks.items.move(moved, after: p) }
    }
    if not moved.done and tasks.items.ancestors(moved, .parent).any(a -> a.done) {
        check(moved, false)
    }
}

/// Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does:
/// the row over it names the task of its own level it ends up under.
action indent(t: Task, above: Option<Task> (above)) (
    label: "Indent",
    icon: .arrow_right,
    key: "Ctrl+]",
) {
    applies above != .none and above != t.parent "Nothing over it to move under"
    if let a = above {
        let under = tasks.items.ancestors(a, .parent).find(x -> x.parent == t.parent) ?? a
        move_to(tasks, t, .some(under), .none)
    }
}

/// Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline.
action outdent(t: Task) (label: "Outdent", icon: .arrow_left, key: "Ctrl+[") {
    applies t.parent != .none "It is at the top level already"
    if let p = t.parent {
        t.parent = p.parent
        tasks.items.move(t, after: p)
    }
}

/// Copies a task, with every task under it, just after the one it copies.
action duplicate(tasks: Tasks, t: Task) -> Task (
    label: "Duplicate",
    icon: .copy,
    key: "Ctrl+Shift+D",
) {
    let copy = tasks.items.insert(copied(t, t.parent))
    if let last = last_below(tasks.items, t) {
        tasks.items.move(copy, after: last)
    } else { tasks.items.move(copy, after: t) }
    copy_below(tasks, t, copy)
    copy
}

/// Deletes a task and its subtasks.
destructive action remove(tasks: Tasks, t: Task) (label: "Delete", icon: .trash, key: "Delete") {
    for x in tasks.items.descendants(t, .parent) {
        tasks.items.remove(x)
    }
    tasks.items.remove(t)
}

/// Deletes every finished task, and answers how many went: a task is finished when everything
/// under it is done too, so clearing takes whole finished branches and never unfinished work
/// with it, and a task done with a step still open under it stays.
destructive action clear_done(tasks: Tasks) -> Int (icon: .circle_check, label: "Clear done") {
    require tasks.items.count(x -> x.done) > 0 "Nothing is done yet"
    // A branch is finished when it and everything under it is done, which the list's index
    // works out for each task: nothing here walks what is under one.
    let gone = tasks.items.filter(x -> x.finished)
    for x in gone {
        tasks.items.remove(x)
    }
    gone.len()
}

/// Deletes a project and its sections; its tasks go back to the inbox.
destructive action remove_project(tasks: Tasks, p: Project) (
    icon: .trash,
    label: "Delete project",
) {
    for t in tasks.items.filter(t -> t.project == p) {
        t.project = .none
        t.section = .none
    }
    tasks.sections.remove_where(s -> s.project == p)
    tasks.projects.remove(p)
}

/// Shows a project's tasks as a board, a lane per section, or as a list again. It is a switch a
/// project's window keeps beside its New task, not a command reached for occasionally.
action show_board(p: Project, on: Bool = not p.board) (
    label: "Board",
    icon: .layout_grid,
    on: p.board,
    frequent: true,
) {
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

/// Adds a task to a project, at the end of the lane `s` of its board (none for the first), under
/// a title a program or an agent gives it. It is a project's primary action, so its key and its
/// menu row bring the person to the entry that adds in that lane, where they type the title.
primary action add_to(p: Project, title: Text, s: Option<Section> = .none) -> Task (
    icon: .plus,
    label: "New task",
    key: "Ctrl+N",
) {
    tasks.items.insert(Task(title: title, project: p, section: s))
}

/// Puts a task into a lane of its project's board, `s` (none for the first lane), at the place
/// `at` among the tasks already there. It is what a card's drag names.
internal action place(t: Task, s: Option<Section>, at: Int) {
    t.section = s
    tasks.items.move(
        t,
        to: at,
        among: tasks.items.filter(x -> x.project == t.project and x.section == s and not x.done),
    )
}

/// Renames a project.
action rename(p: Project, name: Text) {
    p.name = name
}

/// Whether `place` is `top` or one of the tasks over it.
fn within(items: List<Task>, place: Option<Task>, top: Task) -> Bool {
    if let p = place {
        p == top or items.ancestors(p, .parent).any(a -> a == top)
    } else { false }
}

/// `t` as a task of its own under `parent`: what a copy of it says, without the tasks under it.
fn copied(t: Task, parent: Option<Task>) -> Task {
    Task(
        title: t.title,
        done: t.done,
        due: t.due,
        at: t.at,
        notes: t.notes,
        assignee: t.assignee,
        project: t.project,
        section: t.section,
        priority: t.priority,
        parent: parent,
    )
}

/// The task that ends the run `t` begins: the last task of the list that stands under `t`,
/// `t` itself when none does.
fn last_below(items: List<Task>, t: Task) -> Option<Task> {
    items.filter(x -> x == t or items.ancestors(x, .parent).any(a -> a == t)).last()
}

/// Copies the tasks under `t` under `copy`, each in its place, so a copy reads as the task it
/// copies did; answers the last task the copies end with. It is what Duplicate runs, not a
/// command of its own.
internal action copy_below(tasks: Tasks, t: Task, copy: Task) -> Task {
    for k in tasks.items.filter(x -> x.parent == .some(t)) {
        // Where the copy of this one goes: after the run the copies under `copy` make so far,
        // read before it is inserted, since it would end that run itself.
        let after = last_below(tasks.items, copy)
        let made = tasks.items.insert(copied(k, .some(copy)))
        if let last = after { tasks.items.move(made, after: last) }
        copy_below(tasks, k, made)
    }
    last_below(tasks.items, copy) ?? copy
}

/// How many of a task's subtasks are done, of all of them ("1/3"): its row in a list and its
/// node in the page's outline read the two counts the list's index keeps.
fn subtask_count(t: Task) -> Option<Text> {
    if t.subtasks == 0 { .none } else { .some("{t.subtasks_done}/{t.subtasks}") }
}

/// When a task is due of its own, beside what the row it stands under already says: a subtask
/// due when its parent is due repeats the parent's fact, so only a day that differs reads on it.
fn own_due(t: Task, under: Option<Task>) -> Option<Date> {
    if let p = under {
        if p.due == t.due { .none } else { t.due }
    } else { t.due }
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

/// The tasks window: Inbox, Today, Upcoming and Done, each the place its role makes of the
/// tasks (`inbox`, `agenda`, `upcoming`, `logbook`), and the projects, each showing its tasks
/// where they are. The place chosen is the window's, so everyone looking sees the same list;
/// what is typed in the entry at its end is each person's own until Enter adds it.
view home(t: Tasks) {
    window place: Text = "is:inbox"
    // The sidebar's places are the schema's: this type's filters that are on or off, and a
    // place per project. A priority is picked in search, not chosen as a place.
    let side = places(Task, in: t)
    let chosen = side.places.find(p -> p.key == place)
    // A place with a role comes in the order its role says; any other list, by the day.
    let role = chosen?.role ?? ""
    let found = t.items.matching(place)
    let shown = if role == "" { found.sort_by(.due) } else { found }
    let title = chosen?.label ?? "Tasks"
    // A role that counts says how many wait in it; the list says how many it holds.
    let count = if chosen?.counts ?? false { chosen?.count ?? 0 } else { shown.len() }
    // A project's place adds to that project, in the place's own list.
    let project = if chosen?.group != .none { t.projects.find(p -> p.name == title) } else { .none }
    // The window's primary action is the one whose key puts the keyboard in the entry at the
    // list's end, so it is handed to the rows the entry stands under.
    let primary = actions(t).find(a -> a.primary and a.available)
    // The entry says where what is typed will go.
    let entry_placeholder = if let p = project {
        "New task in {p.name}"
    } else if place == "is:today" { "New task for today" } else { "New task" }
    ui.NavShell(
        content_kind: .document,
        sidebar_title: "Tasks",
        overview_title: title,
        overview_count: count,
    ) {
        // What the place leaves open, as its sidebar says it: the rows of what is done with
        // fold under the list, not into its count.
        sidebar: {
            std.sidebar(side, place)
        }
        overview_actions: {
            std.header(actions(t), entry: true)
        }
        overview: {
            std.rows(
                shown,
                "No tasks here",
                primary,
                .some("list-checks"),
                group: .due,
                closed: .folded,
                role: role,
                place: place,
                parent: .parent,
                parent_title: x -> x.parent?.title,
                progress: x -> subtask_count(x),
                all_done: if let c = chosen { c.count == 0 } else { false },
                entry: text -> quick_add(t, text, project, place == "is:today"),
                entry_placeholder: entry_placeholder,
            ) { (x, under) ->
                task_row(x, .day, under)
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
    if p.board {
        project_board(p)
    } else {
        project_list(p)
    }
}

/// A project's tasks as a list, with the entry that adds to it at their foot.
view project_list(p: Project) {
    let shown = tasks_of(p)
    let primary = actions(p).find(a -> a.primary and a.available)
    ui.NavShell(content_kind: .document, overview_title: p.name, overview_count: shown.len()) {
        overview_actions: {
            std.header(actions(p), entry: true)
        }
        overview: {
            std.rows(
                shown,
                "No tasks yet",
                primary,
                .some("folder"),
                parent: .parent,
                parent_title: x -> x.parent?.title,
                progress: x -> subtask_count(x),
                entry: text -> quick_add(tasks, text, p),
                entry_placeholder: "New task in {p.name}",
            ) { (x, under) ->
                task_row(x, .project, under)
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
    // A lane adds at its foot, in its own place: typing there puts the task in that lane.
    let primary = actions(p).find(a -> a.primary and a.available)
    std.page(p, canvas: true, fill: true, entry: true) {
        ui.Scroll(across: true) {
            ui.Board(name: p.name) {
                let loose = lane_of(p, .none)
                ui.BoardLane(title: "No section", count: loose.count()) {
                    on drop(moved: Task, at: Int): place(moved, .none, at)
                    foot: {
                        std.add_entry(primary, "New task", text -> quick_add(tasks, text, .some(p)))
                    }
                    for t in loose {
                        task_card(t, sections)
                    }
                }
                for s in sections {
                    let cards = lane_of(p, .some(s))
                    ui.BoardLane(s, title: s.name, count: cards.count()) {
                        on drop(moved: Task, at: Int): place(moved, .some(s), at)
                        foot: {
                            std.add_entry(
                                primary,
                                "New task",
                                text -> quick_add(tasks, text, .some(p), false, .some(s)),
                            )
                        }
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
            for a in actions(t).filter(a -> a.applies) {
                ui.MenuItem(
                    a.label,
                    icon: a.icon,
                    destructive: a.destructive,
                    key: a.key,
                    disabled: not a.available,
                    reason: if a.available { .none } else { .some(a.reason) },
                    group: if a.frequent { .none } else { a.group },
                ) {
                    on choose: a.run
                }
            }
        }
        task_row(t, .project, .none)
    }
}

/// One task as a row: whether it is done, its title, its project, when it is due, a flag while
/// it matters, and who is doing it. `place` and `under` leave out what the place the row stands
/// in and the row it stands under already say — the day a list of a day is grouped by, the
/// project a project's list is of, the day a subtask shares with its parent — so a row repeats
/// nothing the page around it states.
view task_row(t: Task, place: RowPlace, under: Option<Task>) {
    let project: Option<Text> = if place == .project { .none } else { t.project?.name }
    let due: Option<Date> = if place == .day { .none } else { own_due(t, under) }
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: project,
        meta: due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// A task as the system shows it on a surface of its own — a result, a notice — where no page
/// around it says anything: its row with every fact.
view task_summary(t: Task) {
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: t.project?.name,
        meta: t.due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// One task in an outline, which counts its done subtasks while it is folded. `project` is the
/// project of the page around the outline: a row repeats no project the page already names, and
/// names its own while the subtask is of another. `under` is the task it stands under, whose
/// own day a subtask due the same day does not repeat.
view task_outline_row(t: Task, project: Option<Project>, under: Option<Task>) {
    ui.TreeViewRow(
        checkbox: t.done,
        icon: t.priority.icon(),
        secondary: if t.project == project { .none } else { t.project?.name },
        trailing: own_due(t, under),
    )
}

/// A task's window: its title and whether it is done, what is written about it, its subtasks,
/// its facts — when it is due, its project, how much it matters, who is doing it — as chips,
/// and what happened to it.
view task_page(t: Task) {
    lens step: Text
    std.document(t) {
        opening(t) {
            on check: check(t)
        }
        // What follows the heading stands in its body, under the text its title leads to.
        std.body() {
            section("Subtasks") {
                // A task with no subtasks puts no tree up, and the walk over the tasks under
                // this one is made only once there is one.
                if not tasks.items.filter(x -> x.parent == .some(t)).is_empty() {
                    let kids = tasks.items.descendants(t, .parent)
                    std.outline(
                        "Subtasks",
                        kids,
                        x -> x.parent,
                        x -> x.title,
                        "No subtasks yet",
                        (moved, parent, before) -> move_to(
                            tasks,
                            moved,
                            parent,
                            before,
                        ),
                        x -> check(x),
                        progress: x -> subtask_count(x),
                        fill: false,
                    ) { (x, under) ->
                        task_outline_row(x, t.project, under)
                    }
                }
                ui.TextInput(value: step, placeholder: "Add a subtask") {
                    on submit: add_subtask(tasks, t, step).then(set(step, ""))
                }
            }
            facts(t) {
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
                ui.MenuButton(t.priority.label(), icon: .flag) {
                    for a in actions(t).filter(a -> a.group == .some("Priority")) {
                        ui.MenuItem(
                            a.label,
                            icon: a.icon,
                            disabled: not a.available,
                            reason: if a.available { .none } else { .some(a.reason) },
                        ) {
                            on choose: a.run
                        }
                    }
                }
            }
            std.history(t)
        }
    }
}

/// A household and a small team in the same week: an inbox to sort, two projects, some of it
/// due today, some of it done.
scenario "a busy week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(
        Project(
            name: "Move flat",
            notes: RichText("Keys on the 15th. The van is booked for the morning."),
        ),
    )
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    tasks.items.insert(Task(title: "Take the bins out", done: true))
    let milk = tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    tasks.items.insert(
        Task(
            title: "Check what is in the fridge",
            parent: .some(milk),
            due: 2026-06-30,
            done: true,
        ),
    )
    // What no one has sorted yet: no project, no day.
    tasks.items.insert(Task(title: "Sort the paper mail"))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            notes: RichText("Ask whether Tuesday morning works for the engineer."),
            due: 2026-06-30,
            at: 2026-06-30T09:00,
            priority: .high,
        ),
    )
    let service = tasks.items.insert(
        Task(title: "Find the last service date", parent: .some(call), due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Look through the email folder", parent: .some(service), due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Ask whether Tuesday morning works", parent: .some(call), due: 2026-06-30),
    )
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Reply to Mia about Sunday lunch", due: 2026-07-01))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    tasks.items.insert(
        Task(
            title: "Give notice to the electricity company",
            project: flat,
            due: 2026-07-04,
            priority: .medium,
        ),
    )
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Pack the books", project: flat, done: true))
    let copy = tasks.items.insert(
        Task(
            title: "Write the launch post",
            project: launch,
            due: 2026-07-06,
            priority: .urgent,
            notes: RichText("Lead with the offline editor, then sharing."),
            assignee: .some(mia),
        ),
    )
    // Checked off before the step under it was added: a list written by the version that kept
    // steps apart, or a check made while someone else added that step. Clearing keeps the step.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: .some(copy), project: launch),
    )
    tasks.items.insert(
        Task(title: "Collect the beta quotes", parent: .some(outline), project: launch),
    )
    tasks.items.insert(Task(title: "First draft", parent: .some(copy), project: launch))
    tasks.items.insert(Task(title: "Screenshots", parent: .some(copy), project: launch))
    tasks.items.insert(
        Task(title: "Check the pricing page on phones", project: launch, due: 2026-07-08),
    )
    tasks.items.insert(Task(title: "Set up the redirects", project: launch, done: true))
    call
}

/// A launch post half written: its window, with the subtasks it was broken into, one of them
/// broken into its own.
scenario "a task's subtasks" -> Task {
    let mia = person("Mia")
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    let copy = tasks.items.insert(
        Task(
            title: "Write the launch post",
            project: launch,
            due: 2026-07-06,
            priority: .urgent,
            notes: RichText("Lead with the offline editor, then sharing."),
            assignee: .some(mia),
        ),
    )
    // Checked off before the step under it was added: a list written by the version that kept
    // steps apart, or a check made while someone else added that step. Clearing keeps the step.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: .some(copy), project: launch),
    )
    tasks.items.insert(
        Task(
            title: "Collect the quotes from the beta testers",
            parent: .some(outline),
            project: launch,
        ),
    )
    tasks.items.insert(
        Task(
            title: "First draft, with the quotes from the beta testers",
            parent: .some(copy),
            project: launch,
        ),
    )
    tasks.items.insert(Task(title: "Screenshots", parent: .some(copy), project: launch))
    copy
}

/// A task added but not yet named: its row is the field its title is typed in.
scenario "a task being named" {
    tasks.items.insert(Task(title: "Book the van", due: 2026-07-10))
    tasks.items.insert(Task(title: ""))
}

/// A week everyone has finished.
scenario "all done" {
    tasks.items.insert(Task(title: "Passports", done: true))
    tasks.items.insert(Task(title: "Chargers", done: true))
    tasks.items.insert(Task(title: "Sun cream", done: true))
}

/// A task the day went by on: its page says which day it was due, that it is late, and the
/// steps under it, one of them due the same day as the task it is under and one of them not.
scenario "a task that is late" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            notes: RichText("Ask whether Tuesday morning works for the engineer."),
            project: flat,
            due: 2026-06-29,
            at: 2026-06-29T09:00,
            priority: .high,
        ),
    )
    tasks.items.insert(
        Task(
            title: "Find the last service date",
            parent: .some(call),
            project: flat,
            due: 2026-06-29,
        ),
    )
    tasks.items.insert(
        Task(
            title: "Book the engineer's visit",
            parent: .some(call),
            project: flat,
            due: 2026-07-02,
        ),
    )
    call
}

/// A move a week away, on its project's board: a lane per stage, the tasks in each.
scenario "a project board" -> Project {
    let flat = tasks.projects.insert(
        Project(name: "Move flat", notes: RichText("Keys on the 15th."), board: true),
    )
    let packing = tasks.sections.insert(Section(name: "Packing", project: flat))
    let booked = tasks.sections.insert(Section(name: "Booked", project: flat))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Ask Mia about the sofa", project: flat, due: 2026-07-01))
    tasks.items.insert(Task(title: "Ring the letting agent", project: flat, due: 2026-06-29))
    tasks.items.insert(
        Task(
            title: "Order boxes and tape",
            project: flat,
            section: packing,
            due: 2026-07-02,
            priority: .medium,
        ),
    )
    tasks.items.insert(Task(title: "Pack the books", project: flat, section: packing, done: true))
    tasks.items.insert(
        Task(title: "Pack the kitchen", project: flat, section: packing, due: 2026-07-10),
    )
    tasks.items.insert(Task(title: "Book the van", project: flat, section: booked, priority: .high))
    tasks.items.insert(
        Task(
            title: "Give notice to the electricity company",
            project: flat,
            section: booked,
            due: 2026-07-04,
        ),
    )
    flat
}
