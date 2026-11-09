app so.tilo.lang.tasks "Tasks" (
    icon: .list_checks,
    category: .office,
    previously: so.tilo.lang.todo,
)

use so.tilo.lang.discussion as discussion

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
    /// The day it is grouped under, as a list of a day is.
    day
    /// Its project, as a project's list is of one.
    project
}

/// Something to do: what, by when, by whom, and in which project.
type Task {
    title: Text
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// The moment on that day it is due at, if it has one.
    at: Option<Time> = .none
    /// When to tell whoever is doing it that its moment came, if a reminder is set.
    remind: Option<Time> = .none
    /// Anything worth remembering about it.
    notes: RichText
    /// Who is doing it.
    assignee: Option<Principal> = .none
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    /// The lane of its project's board it is in; none for the first lane.
    section: Option<Section> = .none
    priority: Priority = .none
    /// The task it is a subtask of, to any depth; none for a task of its own.
    parent: Option<Task> = .none

    search {
        filter "Inbox" = project == .none and due == .none and not done (icon: .inbox)
        // What waits for the person looking: what is due today or late, and what is theirs to do
        // whenever it is due, or due at no day at all.
        filter "Today" = not done and (assignee == workspace.me or due <= today) (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter "Done" = done (icon: .circle_check)
        filter "Assigned to me" = assignee == workspace.me and not done (icon: .user)
        filter due
        filter project
        filter priority
        filter parent
        hide at
        hide section
    }
}

/// A project: tasks that work towards one outcome.
type Project {
    name: Text
    /// What it is for, and anything worth remembering about it.
    notes: RichText
}

/// A section of a project: one lane of its board, such as "Doing".
type Section {
    name: Text
    /// The project it is a section of.
    project: Project
}

/// The workspace's tasks and projects.
type Tasks {
    /// Every task, in the order people put them.
    items: Collection<Task>
    projects: Collection<Project>
    /// The sections of every project, in the order its board shows them.
    sections: Collection<Section>
}

root tasks: Tasks

default view home for Tasks

default view task_page for Task

default view project_list for Project

views project_list (icon: .list), project_board (icon: .columns_2) for Project

row view task_summary for Task

/// Adds a task to the inbox or to `p`, in the lane `s` of its board: the entry at a list's end
/// fills `title` with the words nothing else there takes, and the day, moment and person they
/// name, so "Call the landlord fri 10am @Bo" is a task due Friday at ten, for Bo.
primary action new_task(
    tasks: Tasks,
    p: Option<Project> = .none,
    s: Option<Section> = .none,
    due: Option<Date> = .none,
    at: Option<Time> = .none,
    assignee: Option<Principal> = .none,
    title: Text = "",
) -> Task (icon: .plus, label: "New task", key: "Ctrl+N") {
    tasks.items.insert(
        Task(title: title, due: due, at: at, assignee: assignee, project: p, section: s),
    )
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

/// Adds a subtask under a task, in its project and section, and opens a done one: there is work
/// under it now.
action add_subtask(tasks: Tasks, t: Task, title: Text) -> Task uses time (
    label: "Add subtask",
    icon: .list_tree,
    did: "added a subtask",
    key: "Ctrl+Shift+N",
) {
    let made = tasks.items.insert(
        Task(title: title, parent: t, project: t.project, section: t.section),
    )
    let last = tasks.items.filter(x -> x.parent == t and x != made).last() ?? t
    tasks.items.move(made, after: last)
    if t.done or tasks.items.ancestors(t, .parent).any(a -> a.done) { check(t, false) }
    made
}

/// Sets the day a task is due by, from the presets or a day the person picks.
action due_today(t: Task) uses time (label: "Today", icon: .star, key: "Ctrl+T", group: "Due") {
    require not t.done "It is done already"
    t.due = today
}

/// Makes a task due tomorrow.
action due_tomorrow(t: Task) uses time (label: "Tomorrow", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.due = today + 1d
}

/// Makes a task due at the start of next week.
action due_next_week(t: Task) uses time (label: "Next week", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.due = today.start_of_week() + 1w
}

/// Makes a task due on a day the person picks.
action schedule(t: Task, due: Option<Date>) (
    label: "Pick date",
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

/// Rings a task's reminder when its moment comes, with no window of this app open: the
/// workspace's own device runs it, once for all of them.
trigger ring(t: Task) at t.remind: remind(t)

/// Sets the moment whoever is doing a task is reminded of it.
action remind_at(t: Task, when: Time) uses time (label: "Remind me", icon: .bell, group: "Remind") {
    t.remind = when
}

/// Clears a task's reminder: it is not told of again.
action remind_clear(t: Task) uses time (label: "No reminder", icon: .bell_off, group: "Remind") {
    require t.remind != .none "It has no reminder"
    t.remind = .none
}

/// Tells whoever is doing a task, or the one who made it, what it is and when it is due: once,
/// and not of a task already done. Choosing the notice opens the task.
action remind(t: Task) uses notify {
    t.remind = .none
    if t.done {
        return
    }
    let who: Option<Principal> = if let a = t.assignee {
        a
    } else if let made = created(t) { made.who }
    if let w = who {
        notify(
            to: w,
            title: t.title,
            body: if let d = t.due { "Due {d}" } else if let p = t.project { p.name } else { "" },
            opens: t,
        )
    }
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
    label: "Project",
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
action assign(t: Task, who: Option<Principal>) (label: "Assign", icon: .user) {
    t.assignee = who
}

/// Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for
/// none), or where `before` stands. What is put under a done task opens it.
action move_to(tasks: Tasks, moved: Task, parent: Option<Task>, before: Option<Task>) uses time {
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

/// Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does.
action indent(t: Task, above: Option<Task> (above)) uses time (
    label: "Indent",
    icon: .arrow_right,
    key: "Ctrl+]",
) {
    applies above != .none and above != t.parent "Nothing over it to move under"
    if let a = above {
        let under = tasks.items.ancestors(a, .parent).find(x -> x.parent == t.parent) ?? a
        move_to(tasks, t, under, .none)
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
    let under = tasks.items.descendants(t, .parent)
    let copy = tasks.items.insert(copied(t, t.parent))
    if let last = last_below(tasks.items, t) {
        tasks.items.move(copy, after: last)
    } else { tasks.items.move(copy, after: t) }
    // The copies stand in the shape of the tasks they copy, in the order they are made: a task's
    // copy hangs under the copy of the task it stands under, at the place it takes itself.
    for x in under {
        let under_copy = under.index_of(x.parent ?? t) ?? -1
        let at = tasks.items.descendants(copy, .parent).at(under_copy) ?? copy
        let below = last_below(tasks.items, at) ?? at
        let made = tasks.items.insert(copied(x, at))
        tasks.items.move(made, after: below)
    }
    copy
}

/// Deletes a task and its subtasks.
destructive action remove(tasks: Tasks, t: Task) (label: "Delete", icon: .trash, key: "Delete") {
    for x in tasks.items.descendants(t, .parent) {
        tasks.items.remove(x)
    }
    tasks.items.remove(t)
}

/// Deletes every finished task and answers how many went: a finished branch goes whole, and a
/// done task with a step still open under it stays. What was done is the list a person reads
/// back, so this is no command of the bar.
destructive action clear_done(tasks: Tasks) -> Int (
    icon: .circle_check,
    label: "Delete finished tasks",
) {
    // A task that is done stands in a finished branch or under unfinished work: the entry says
    // nothing of it, so the guard is every task's own mark, read once.
    require tasks.items.count(x -> x.done) > 0 "Nothing is finished yet"
    let every = tasks.items.filter(x -> true)
    let gone = every.filter(x -> finished(every, x))
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

/// Adds a task to a project, at the end of the lane `s` of its board (none for the first). It is the
/// project's primary action, so its key brings the person to the entry that adds there.
primary action add_to(p: Project, title: Text, s: Option<Section> = .none) -> Task (
    icon: .plus,
    label: "New task",
    key: "Ctrl+N",
) {
    tasks.items.insert(Task(title: title, project: p, section: s))
}

/// Puts a task into the lane `s` of its project's board (none for the first) at the place `at`
/// among the tasks already there: what a card's drag names.
action place(t: Task, s: Option<Section>, at: Int) (label: "Place in board") {
    require s == .none or s?.project == t.project "That lane belongs to another project"
    applies t.project != .none "It is in no project's board"
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

/// The task that ends the run `t` begins: the last task under `t`, or `t` itself.
fn last_below(items: List<Task>, t: Task) -> Option<Task> {
    items.filter(x -> x == t or items.ancestors(x, .parent).any(a -> a == t)).last()
}

/// Whether `t` is finished: done itself, and every task under it. Read over `items`, the list
/// the caller holds, so a branch two people finish at once is finished once both changes are in.
fn finished(items: List<Task>, t: Task) -> Bool {
    let under = items.descendants(t, .parent)
    if under.is_empty() { t.done } else { under.all(x -> x.done) }
}

/// Whether `t` stands in a list's Done tail: finished, and so is everything over it, so a step
/// done under a task still open stays where it stands, checked and muted.
fn folded(items: List<Task>, t: Task) -> Bool {
    finished(items, t) and items.ancestors(t, .parent).all(a -> finished(items, a))
}

/// How many of a task's subtasks are done, of all of them ("1/3"), from the tasks that name a
/// parent, read once for the view around them. None while the window has not read the whole
/// workspace: what it holds so far is not the whole.
fn subtask_count(kids: List<Task>, t: Task) -> Option<Text> {
    let mine = kids.filter(x -> x.parent == t)
    if let total = mine.counted() {
        if not mine.is_empty() { "{mine.count(x -> x.done)}/{total}" }
    }
}

/// When a task is due of its own: a subtask repeats no day its parent already says.
fn own_due(t: Task, under: Option<Task>) -> Option<Date> {
    if let p = under { if p.due != t.due { t.due } } else { t.due }
}

/// When a task was finished: the moment its box was last checked off, read from the index, one
/// value a task, never its history.
fn finished_at(t: Task) -> Option<Time> {
    if let v = changed(t, .done) { if t.done { v.at } }
}

/// The day a task stands under in a place's list: the day it is due on, or in Done the day it
/// was finished on.
fn task_day(t: Task, by_day: Bool, done_place: Bool) -> Option<Date> {
    if by_day { t.due } else if done_place { finished_at(t)?.day() }
}

/// The tasks of a project, in order.
fn tasks_of(p: Project) -> List<Task> {
    tasks.items.filter(t -> t.project == p)
}

/// The sections of a project, in order.
fn sections_of(p: Project) -> List<Section> {
    tasks.sections.filter(s -> s.project == p)
}

/// The open tasks of one lane of a project's board: those of the section `s`, or of none. A
/// subtask is not a card; it goes where the task it is under goes.
fn lane_of(p: Project, s: Option<Section>) -> List<Task> {
    tasks.items.filter(t -> t.project == p and t.section == s and not t.done and t.parent == .none)
}

/// How many wait somewhere, as a badge shows it: none while nothing waits, and none before the
/// window has read the whole workspace, so a part read so far never reads as a count.
fn waiting(count: Option<Int>) -> Option<Int> {
    if let n = count { if n > 0 { count } }
}

/// The places a window offers, in the order it offers them: what is assigned to the person
/// looking is one only where someone else may be assigned it too.
fn places(shared: Bool) -> List<Filter<Task>> {
    if shared {
        [Task.inbox, Task.today, Task.upcoming, Task.done, Task.assigned_to_me]
    } else {
        [Task.inbox, Task.today, Task.upcoming, Task.done]
    }
}

/// The tasks a list shows, with the one held open where it stands: a row stays while it is open,
/// so what a person just changed does not vanish from under them.
fn holding(listed: List<Task>, opened: Option<Task>) -> List<Task> {
    if let o = opened {
        if listed.contains(o) {
            listed
        } else {
            tasks.items.filter(x -> x == o or listed.contains(x))
        }
    } else { listed }
}

/// The place a task stands in: its project's, or the first of `side` that holds it, else Inbox.
fn place_of(side: List<Filter<Task>>, x: Task) -> Filter<Task> {
    if let p = x.project {
        Task.project(p)
    } else {
        side.find(f -> tasks.items.matching(f).any(t -> t == x)) ?? Task.inbox
    }
}

/// The tasks window: Inbox, Today, Upcoming, Done, what is assigned to the person looking, and the
/// projects. A window's state is its person's own, and so is what its entry holds until Enter.
view home(t: Tasks) {
    state place: Filter<Task> = Task.today
    state opened: Option<Task> = .none
    on show(x: Task): set(place, place_of(places(workspace.members.len() > 1), x))
        .then(set(opened, x))
    let side = places(workspace.members.len() > 1)
    let chosen = side.find(f -> f == place)
    let found = holding(t.items.matching(place), opened)
    let project = t.projects.find(p -> place == Task.project(p))
    // A place of days groups by day and a project's list folds finished work under its foot.
    let by_day = place == Task.today or place == Task.upcoming or project != .none
    let done_place = place == Task.done
    let folds = project != .none
    let in_today = place == Task.today
    // Due soonest first, Done newest finished first, a project's list with finished work last.
    let ordered = found.sort_by(.due)
    // The tasks that name a parent, read once: a row counts its own under them.
    let kids = found.filter(x -> x.parent != .none)
    let shown = if done_place {
        found.sort_by(x -> changed(x, .done)?.at, desc)
    } else if folds { ordered.sort_by(x -> folded(found, x)) } else { ordered }
    let title = if let p = project { p.name } else { chosen?.label ?? "Tasks" }
    // What waits in the place, read from the index rather than walked: three tasks or three
    // thousand answer as fast.
    let open: Int = if let p = project {
        found.len() - t.items.matching(Task.project(p), Task.done).len()
    } else { found.len() }
    let all_done = open == 0 and not found.is_empty()
    // The places whose badge counts what waits there: what a person goes to, not a list read
    // through, and a count only once the window has read the whole workspace ([`waiting`]).
    let counted: List<Text> = ["inbox", "today", "assigned_to_me"]
    let header_count = if let c = chosen {
        if counted.any(k -> k == c.key) { waiting(found.counted()) }
    }
    let entry_placeholder = if let p = project {
        "New task in {p.name}"
    } else if place == Task.today { "New task for today" } else { "New task" }
    ui.NavShell(
        content_kind: .list,
        sidebar_title: "Tasks",
        title: title,
        count: header_count,
        actions: [new_task, clear_done],
    ) {
        sidebar: {
            ui.NavList(selected: place) {
                for f in side {
                    let badge = if counted.any(k -> k == f.key) {
                        waiting(t.items.matching(f).counted())
                    }
                    ui.NavItem(f, f.label, icon: f.icon, count: badge) {
                        on select: set(place, f)
                    }
                }
                ui.NavSection("Projects", add: new_project(t)) {
                    for p in t.projects {
                        ui.NavItem(Task.project(p), p.name, icon: .folder) {
                            on select: set(place, Task.project(p))
                        }
                    }
                }
            }
        }
        // A task opens where it stands in the list, not in a window of its own.
        content: {
            page(fill) {
                ui.Docked {
                    foot: {
                        stack {
                            if all_done {
                                task_list(shown, kids, by_day, done_place, folds, opened, .day)
                            }
                            // A task typed in Today that names no day is due today, so it stands
                            // where it was typed.
                            let add = new_task(t, p: project, due: if in_today { today })
                            std.add_entry(entry_placeholder, add)
                        }
                    }
                    if all_done {
                        ui.EmptyState(phrase("tilolang-all-done"), icon: .list_checks, inline: true)
                    } else if shown.is_empty() {
                        ui.EmptyState("No tasks here", icon: .list_checks)
                    } else {
                        task_list(shown, kids, by_day, done_place, folds, opened, .day)
                    }
                }
            }
        }
    }
}

/// The tasks of a place as a list: each branch nested under the task it is a subtask of, the
/// finished work under the foot `folds` when it says so.
view task_list(
    items: List<Task>,
    kids: List<Task>,
    by_day: Bool,
    done_place: Bool,
    folds: Bool,
    opened: State<Option<Task>>,
    place: RowPlace,
) {
    std.list(
        items,
        opened,
        tree: true,
        ungrouped: if by_day { phrase("tilolang-no-date") },
        tail_title: if folds { phrase("tilolang-done") },
    ) { (x, at) ->
        let heading = task_day(x, by_day, done_place)
        let tail = folds and folded(items, x)
        // The task just over this one in its group, on this side of the fold: what `indent`
        // moves under, and none for the first of a group.
        let above = if let p = items.previous(x) {
            if task_day(p, by_day, done_place) == heading and (folds and folded(items, p)) == tail {
                p
            }
        }
        task_item(x, at, place, heading, done_place, tail, above, subtask_count(kids, x))
    }
}

/// One task as a row of a list, and the document that opens into it where it stands.
view task_item(
    t: Task,
    at: Option<Principal>,
    place: RowPlace,
    group: Option<Date>,
    group_done: Bool,
    tail: Bool,
    above: Option<Task>,
    progress: Option<Text>,
) {
    ui.ListItem(
        item: t,
        presence: at,
        group: group,
        group_done: group_done,
        tail: tail,
        above: above,
        parent: t.parent,
        parent_title: t.parent?.title,
        progress: progress,
        actions: [
            check(primary: true),
            schedule(group: "Plan"),
            assign,
            indent(group: "Move"),
            outdent(group: "Move"),
            remove,
        ],
    ) {
        task_row(t, place, t.parent)
        detail: {
            task_body(t)
        }
        menu: {
            ui.MenuItem("Open as page", icon: .arrow_up_right) {
                on choose: open(t, view: task_page)
            }
        }
    }
}

/// A project's window: its tasks, in order, with the entry that adds to it at their foot. The
/// bar switches it to the project's board, the same tasks as a lane per section.
view project_list(p: Project) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    let shown = tasks_of(p)
    let kids = shown.filter(x -> x.parent != .none)
    let listed = holding(shown, opened)
    ui.NavShell(content_kind: .list, title: p.name) {
        content: {
            page(fill) {
                ui.Docked {
                    foot: {
                        stack {
                            std.add_entry("New task in {p.name}", new_task(tasks, p: p))
                        }
                    }
                    if listed.is_empty() {
                        ui.EmptyState("No tasks yet", icon: .folder)
                    } else {
                        task_list(listed, kids, false, false, false, opened, .project)
                    }
                }
            }
        }
    }
}

/// A project's board: a lane per section, the first for the tasks in none, each open task a card.
view project_board(p: Project) {
    let sections = sections_of(p)
    std.page(p, canvas: true, fill: true) {
        ui.Scroll(across: true) {
            ui.Board(name: p.name) {
                let loose = lane_of(p, .none)
                // No section named: the lane keeps its heading's height so cards line up with a
                // lane beside it, and is not headed where the project has no sections.
                ui.BoardLane {
                    on drop(moved: Task, at: Int): place(moved, .none, at)
                    foot: {
                        std.add_entry("New task", new_task(tasks, p: p))
                    }
                    for t in loose {
                        task_card(t, sections)
                    }
                }
                for s in sections {
                    let cards = lane_of(p, s)
                    ui.BoardLane(s, title: s.name, count: cards.counted()) {
                        on drop(moved: Task, at: Int): place(moved, s, at)
                        foot: {
                            std.add_entry("New task", new_task(tasks, p: p, s: s))
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

/// A task on a board: its row. It opens in a window of its own, and its menu moves it to another
/// lane; the host appends what its type declares after the moves.
view task_card(t: Task, sections: List<Section>) {
    ui.BoardCard(t, actions: [check(primary: true), schedule(group: "Plan"), assign, remove]) {
        on activate: open(t)
        menu: {
            if t.section != .none {
                ui.MenuItem("Remove from section", icon: .arrow_right) {
                    on choose: place(t, .none, 0)
                }
            }
            for s in sections {
                if t.section != s {
                    ui.MenuItem("Move to {s.name}", icon: .arrow_right) {
                        on choose: place(t, s, 0)
                    }
                }
            }
        }
        task_row(t, .project, .none)
    }
}

/// One task as a row: whether it is done, its title, its project, when it is due, a flag while it
/// matters, and who is doing it. `place` and `under` leave out what the list and the row over it
/// already say.
view task_row(t: Task, place: RowPlace, under: Option<Task>) {
    let project = if place != .project { t.project?.name }
    let due = if place != .day { own_due(t, under) }
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: project,
        note: proposal_note(t),
        meta: due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// A task as the system shows it on a surface of its own — a result, a notice — with every fact.
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

/// One task in an outline, counting its done subtasks while it is folded. `project` is the page's
/// project: a row names its own only when it differs, and repeats no day its parent already says.
view task_outline_row(t: Task, project: Option<Project>, under: Option<Task>) {
    ui.TreeViewRow(
        checkbox: t.done,
        icon: t.priority.icon(),
        secondary: if t.project != project { t.project?.name },
        trailing: own_due(t, under),
    )
}

/// A task as a page of its own, outside the list it stands in.
view task_page(t: Task) {
    std.document(t) {
        task_body(t)
    }
}

/// A task where it stands: opened in the list it is an item of, or as a page of its own.
template view task_body(t: Task) {
    state step: Text
    stack(gap: .related) {
        row(gap: .related, align: .start) {
            ui.Checkbox(bind: t.done) {
                on check: check(t)
            }
            ui.TextInput(bind: t.title, placeholder: "What to do", title: true, fill: 1.0)
        }
        std.origin(t)
        stack(lead: 1) {
            ui.BlockEditor(bind: t.notes, placeholder: "Anything worth remembering about it.")
        }
    }
    std.body() {
        std.suggestions(t)
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
            ui.MenuButton(if t.remind == .none { "Remind" } else { "Reminder set" }, icon: .bell) {
                ui.MenuItem("Remind me…", icon: .bell) {
                    on choose: remind_at(t)
                }
                ui.MenuItem("No reminder", icon: .bell_off, checked: t.remind == .none) {
                    on choose: remind_clear(t)
                }
            }
        }
        section("Subtasks") {
            // The walk over the tasks under this one is made only where there is one.
            if not tasks.items.filter(x -> x.parent == t).is_empty() {
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
                    progress: x -> subtask_count(kids, x),
                    fill: false,
                ) { (x, under) ->
                    task_outline_row(x, t.project, under)
                }
            }
            ui.TextInput(bind: step, placeholder: "Add a subtask") {
                on submit: add_subtask(tasks, t, step).then(set(step, ""))
            }
        }
        std.linked(t, without: discussion.said_about(t))
        discussion.thread(t)
        std.history(t)
    }
}

/// A household and a small team in one week: an inbox to sort, two projects, some of it done.
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
        Task(title: "Check what is in the fridge", parent: milk, due: 2026-06-30, done: true),
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
        Task(title: "Find the last service date", parent: call, due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Look through the email folder", parent: service, due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Ask whether Tuesday morning works", parent: call, due: 2026-06-30),
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
            assignee: mia,
        ),
    )
    // Checked off before the subtask under it was added: clearing it keeps the subtask.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: copy, project: launch),
    )
    tasks.items.insert(Task(title: "Collect the beta quotes", parent: outline, project: launch))
    tasks.items.insert(Task(title: "First draft", parent: copy, project: launch))
    tasks.items.insert(Task(title: "Screenshots", parent: copy, project: launch))
    tasks.items.insert(
        Task(title: "Check the pricing page on phones", project: launch, due: 2026-07-08),
    )
    tasks.items.insert(Task(title: "Set up the redirects", project: launch, done: true))
    call
}

/// The week's log: what was finished on each day, newest first. It answers with one of them, whose
/// place is Done, so the window stands in Done with it open where it stands.
scenario "what got done this week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(Task(title: "Book the van", project: flat, due: 2026-07-02))
    let boxes = tasks.items.insert(Task(title: "Order boxes and tape", project: flat))
    let notice = tasks.items.insert(
        Task(title: "Give notice to the electricity company", project: flat),
    )
    let passports = tasks.items.insert(Task(title: "Find the passports"))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    invoke(check(passports, true), by: mia, at: 2026-06-23T12:00Z)
    invoke(check(van, true), by: mia, at: 2026-06-29T12:00Z)
    invoke(check(boxes, true), by: mia, at: 2026-06-29T15:40Z)
    invoke(check(notice, true), by: mia, at: 2026-06-30T07:15Z)
    passports
}

/// Today: what is due, what went by, and what is done with it.
scenario "today" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            due: 2026-06-30,
            at: 2026-06-30T09:00,
            priority: .high,
        ),
    )
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Pack the kitchen", project: flat, due: 2026-06-30))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    // Mine, with no day of its own: it stands in my Today because it is mine.
    tasks.items.insert(Task(title: "Sort the paper mail", assignee: invoker))
    call
}

/// A launch post half written, and the subtasks it was broken into, one of them broken into its own.
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
            assignee: mia,
        ),
    )
    // Checked off before the subtask under it was added: clearing it keeps the subtask.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: copy, project: launch),
    )
    tasks.items.insert(
        Task(title: "Collect the quotes from the beta testers", parent: outline, project: launch),
    )
    tasks.items.insert(
        Task(
            title: "First draft, with the quotes from the beta testers",
            parent: copy,
            project: launch,
        ),
    )
    tasks.items.insert(Task(title: "Screenshots", parent: copy, project: launch))
    copy
}

/// A week a person, a colleague and their agent share: who holds what, and what each did to it.
scenario "a shared week" -> Task {
    let mia = person("Mia")
    let scout = agent("Scout")
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    let copy = tasks.items.insert(
        Task(title: "Write the launch post", project: launch, due: 2026-07-06, assignee: mia),
    )
    let quotes = tasks.items.insert(
        Task(
            title: "Collect the quotes from the beta testers",
            parent: copy,
            project: launch,
            assignee: scout,
        ),
    )
    tasks.items.insert(
        Task(
            title: "Check the pricing page on phones",
            project: launch,
            due: 2026-07-08,
            assignee: invoker,
        ),
    )
    tasks.items.insert(Task(title: "Book the van", due: 2026-07-10, assignee: mia))
    invoke(add_subtask(tasks, copy, "Ask three customers for a quote"), by: scout)
    invoke(check(quotes, true), by: mia)
    copy
}

/// A task added but not yet named: its row is the field its title is typed in.
scenario "a task being named" {
    tasks.items.insert(Task(title: "Book the van", due: 2026-07-10))
    tasks.items.insert(Task(title: ""))
}

/// A week everyone has finished: it answers with one of them, whose place is Done.
scenario "all done" -> Task {
    let passports = tasks.items.insert(Task(title: "Passports", done: true))
    tasks.items.insert(Task(title: "Chargers", done: true))
    tasks.items.insert(Task(title: "Sun cream", done: true))
    passports
}

/// A task the day went by on, with steps under it due the same day and later.
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
        Task(title: "Find the last service date", parent: call, project: flat, due: 2026-06-29),
    )
    tasks.items.insert(
        Task(title: "Book the engineer's visit", parent: call, project: flat, due: 2026-07-02),
    )
    call
}

/// A task whose notes name another one: its page draws the task it names as a card beside it.
scenario "a task naming another" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(
        Task(title: "Book the van", project: flat, due: 2026-07-10, priority: .high),
    )
    tasks.items.insert(
        Task(
            title: "Pack the kitchen",
            project: flat,
            notes: RichText("Put everything in ", van, " before the morning."),
        ),
    )
}

/// A task made from another, which was made from a third: its page names the one it was made from
/// and draws the task made from it as a card.
scenario "a task made from another" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(
        Task(title: "Book the van", project: flat, due: 2026-07-10, priority: .high),
    )
    let pack = invoke(
        tasks.items.insert(
            Task(title: "Pack the kitchen", project: flat, due: 2026-07-08, priority: .high),
        ),
        from: van,
    )
    invoke(
        tasks.items.insert(
            Task(title: "Load the boxes by the door", project: flat, due: 2026-07-09),
        ),
        from: pack,
    )
    pack
}

/// What an agent suggested about a task, before anyone decided it: its page says what the change
/// would be and who proposed it, and its row carries the same in a line. Nothing has changed yet.
scenario "an agent's suggestion" -> Task {
    let scout = agent("Scout")
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(Task(title: "Book the van", project: flat))
    invoke(suggest schedule(van, due: 2026-07-10), by: scout)
    van
}

/// A move a week away, on its project's board: a lane per stage, the tasks in each.
scenario "a project board" -> Project {
    let flat = tasks.projects.insert(
        Project(name: "Move flat", notes: RichText("Keys on the 15th.")),
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

/// A task page with the discussion under it: what two people said about the chairs, days apart.
scenario "a discussion on a task" -> Task {
    let ada = person("Ada")
    let bo = person("Bo")
    let t = tasks.items.insert(
        Task(
            title: "Order the new office chairs",
            notes: RichText("Two for the window desks, and four for the room."),
            due: 2026-07-01,
            assignee: ada,
        ),
    )
    invoke(
        discussion.comment(
            discussion.comments,
            RichText("The supplier quotes six weeks. Do we order before the summer?"),
        ),
        by: ada,
        at: 2026-06-24T09:12Z,
        from: t,
    )
    invoke(
        discussion.comment(discussion.comments, RichText("Yes. The room stays empty either way.")),
        by: bo,
        at: 2026-06-24T15:40Z,
        from: t,
    )
    invoke(
        discussion.comment(
            discussion.comments,
            RichText("Ordered, delivery the first week of August."),
        ),
        by: ada,
        at: 2026-06-25T08:03Z,
        from: t,
    )
    t
}

/// A day away from the week's tasks: while the person looking was out, a colleague and their agent
/// changed three of them, so the header counts what it holds unseen and the rows are marked.
scenario "while you were away" -> Task {
    let ada = invoker
    let bo = person("Bo")
    let scout = agent("Scout")
    let paper = Task(title: "Sort the paper mail")
    let permit = Task(title: "Ask the neighbours about the parking permit")
    let survey = Task(title: "Read the survey responses", due: 2026-07-02)
    let milk = Task(title: "Buy oat milk and eggs", due: 2026-06-30)
    paper = tasks.items.insert(paper)
    permit = tasks.items.insert(permit)
    survey = tasks.items.insert(survey)
    milk = tasks.items.insert(milk)
    invoke(assign(paper, ada), by: bo, at: 2026-06-29T16:20Z)
    invoke(schedule(survey, 2026-06-30), by: scout, at: 2026-06-29T17:05Z)
    invoke(check(milk, true), by: scout, at: 2026-06-29T17:12Z)
    survey
}

/// A task the person looking wrote, which a colleague changed since: its page says the change is
/// unseen, and its history says who made it.
scenario "a task that changed since you looked" -> Task {
    let bo = person("Bo")
    let call = Task(
        title: "Call the landlord about the boiler",
        notes: RichText("Ask whether Tuesday morning works for the engineer."),
        due: 2026-07-06,
        priority: .medium,
    )
    call = tasks.items.insert(call)
    invoke(schedule(call, 2026-06-30), by: bo, at: 2026-06-29T16:32Z)
    call
}
