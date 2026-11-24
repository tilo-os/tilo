app so.tilo.tasks "Tasks" (
    icon: .list_checks,
    category: .office,
    tagline: "Issues of the workspace, on a list and a board",
    keywords: ["tasks", "issues", "issue tracker", "kanban", "board", "to-do", "todo", "tickets", "projects", "agent tasks"],
    splash_budget: 250,
)

use so.tilo.contacts as contacts
use so.tilo.lang.discussion as discussion
use so.tilo.notes as notes
use tilo.workspace as workspace

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

/// Where a task stands in its work: a board's columns, fixed so that every task of the workspace
/// reads in the same place in the same column.
enum Status {
    /// Written down, not planned yet.
    backlog
    /// Planned: the next thing to pick up.
    todo (icon: .circle)
    /// Someone is working on it.
    in_progress (icon: .circle_dot, tone: .info)
    /// Done by whoever worked on it, waiting for a person to look.
    in_review (icon: .eye, tone: .info)
    /// Done and checked by its person.
    done (icon: .circle_check, tone: .success)
    /// Not to be done.
    cancelled (icon: .ban)
}

/// Whether a task is finished, one way or the other.
fn closed(s: Status) -> Bool {
    s == .done or s == .cancelled
}

/// Which kind of project this is.
enum Kind {
    /// A finite goal: the work ends, and so does the project.
    project
    /// An ongoing responsibility that has no end, only what is done next.
    area
}

/// Whose budget a task's work spends.
enum BudgetBy {
    /// Its person's.
    person
    /// The agent working on it, which holds itself to it.
    agent
}

/// What a wait waits for: the kinds a session of an agent tells apart.
enum WaitKind {
    /// A moment: nothing else ends it.
    time
    /// A reply: in a mail thread, a conversation.
    reply
    /// A resource of the workspace changing.
    change
    /// An answer of the person the work is for: the task needs them.
    answer
    /// Answers of several people.
    people
    /// Its pieces, the tasks under it: it goes on when none of them is still to be worked on.
    pieces
}

/// Someone involved in a project, with the role they have there: who a task can be given to
/// without typing them in again. Being named here gives nobody access.
type Person {
    name: Text
    role: Text
}

/// A project or an area: the outcome tasks work towards, and who is involved in it.
type Project {
    name: Text
    kind: Kind = .project
    /// What it is for, and anything worth remembering about it.
    notes: RichText
    people: Collection<Person>
}

/// What a task waits for, in a few words.
type Wait {
    kind: WaitKind = .reply
    /// What it waits for: a reply, a moment, a resource, the pieces.
    what: Text
    /// The latest moment the wait ends, whether or not what it waits for came.
    until: Option<Time> = .none
    /// The automation that ends the wait early when it runs, by its id.
    event: Text = ""
}

/// Why a task's work stopped.
enum StoppedWhy {
    /// It could not go on: the line says what failed.
    failed
    /// Its budget in money is spent.
    budget
    /// Its budget in working time is spent.
    time
    /// Its agent sessions were interrupted and did not come back.
    lost_worker
    /// A person stopped it.
    person
}

/// That a task's work stopped, and why.
type Stopped {
    why: StoppedWhy = .failed
    /// The plain line its person reads.
    words: Text = ""
    at: Time
}

/// What a task's work waited for, and what made it go on.
type Resumed {
    cause: Text
    at: Time
}

/// What a task keeps while an agent works it over many sessions: where its working material is,
/// what it waits for, what it may cost, and how long it worked.
type Work {
    /// The folder of the workspace where its material is kept, by its address.
    folder: Text = ""
    /// The note its sessions write, by its address.
    note: Text = ""
    /// What it waits for: none while it waits for nothing.
    waits: List<Wait>
    /// The latest moment its latest wait ends.
    until: Option<Time> = .none
    /// When the wait began.
    since: Option<Time> = .none
    /// What it may spend, in a millionth of the workspace's money; none for no limit.
    budget_micros: Option<Int> = .none
    /// What it may work, in milliseconds; none for no limit.
    budget_ms: Option<Int> = .none
    /// Whose budget the limits charge.
    budget_by: BudgetBy = .person
    /// What its sessions spent, in a millionth of the workspace's money.
    spent_micros: Int = 0
    /// How long its sessions worked, in milliseconds.
    worked_ms: Int = 0
    /// How many sessions worked it.
    sessions: Int = 0
    /// When the session now working it began.
    session_since: Option<Time> = .none
    /// How many sessions ended without finishing.
    lost: Int = 0
    /// Why its work stopped, with the words it did.
    stopped: Option<Stopped> = .none
    /// What made it go on after a wait.
    resumed: Option<Resumed> = .none
    /// How many of its pieces may be in work at once.
    pieces_at_once: Option<Int> = .none
}

/// Where it was first written, when it was moved here from another tracker or service: the
/// person there, the service, where it reads there and its reference, so a task or a thread
/// imported from elsewhere says where it came from.
type Origin {
    /// Who wrote it there, as they were named.
    author: Text
    /// The name they were known by there, when it differs.
    handle: Option<Text> = .none
    /// The service it came from.
    source: Text
    /// Where it reads there.
    url: Option<Text> = .none
    /// Its reference there, such as its number.
    reference: Option<Text> = .none
    /// When it was first written there.
    created: Option<Time> = .none
    /// When it last changed there.
    updated: Option<Time> = .none
}

/// Something to do: what, by when, by whom, in which project and where in its work.
type Task {
    /// Its number in the workspace (`#12`), given once when it is made.
    number: Int
    title: Text
    notes: RichText
    status: Status = .backlog
    /// Whether it is finished, which its status says: a row's box edits a flag, so the flag the
    /// status means is kept beside it and every action that writes the status writes it too.
    /// Nothing reads it for what is done with: the places and the boards read the status.
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// The moment on that day it is due at, if it has one.
    at: Option<Time> = .none
    /// When to tell whoever is doing it that its moment came, if a reminder is set.
    remind: Option<Time> = .none
    /// Who is doing it: a member of the workspace, a person or an agent.
    assignee: Option<Actor> = .none
    /// Who is doing it when they are not a member: someone the workspace keeps in touch with,
    /// from its Contacts book. A task is a member's or a contact's, never both.
    contact: Option<contacts.Contact> = .none
    /// Who gave it to the agent it is assigned to.
    assigned_by: Option<Actor> = .none
    /// When it was given to that agent.
    assigned_at: Option<Time> = .none
    /// The words people sort it by across projects, such as "bug" or "errand".
    labels: Set<Text>
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    priority: Priority = .none
    /// How much work it is, in points.
    estimate: Option<Int> = .none
    /// The task it is a subtask of, to any depth; none for a task of its own.
    parent: Option<Task> = .none
    /// Files attached to it.
    attachments: List<File>
    /// Where it was first written, when it was moved here from another tracker.
    origin: Option<Origin> = .none
    /// The page it is read at elsewhere.
    url: Option<Text> = .none
    /// What it keeps while an agent works it.
    work: Option<Work> = .none
    /// What it waits for, in a few words: empty while it waits for nothing. The first of its
    /// work's waits, kept here so a place finds it without reading every task's work.
    waiting_for: Text = ""
    /// The latest moment its wait ends, so its wait ends when it comes.
    waiting_until: Option<Time> = .none
    /// The automation that ends its wait early, when one does.
    waiting_on: Text = ""
    /// The tasks this one relates to, with how they relate: where the workspace's links between
    /// tasks are kept.
    related: Collection<Related>

    search {
        filter "All issues" = number >= 0 (icon: .list_tree)
        filter "Inbox" = project == .none
            and due == .none
            and not (status == .done or status == .cancelled) (icon: .inbox)
        // What waits for the person looking: what is due today or late, and what is theirs to do
        // whenever it is due, or due at no day at all.
        filter "Today" = not (status == .done or status == .cancelled)
            and (assignee == invoker or due <= today) (icon: .star)
        filter "Upcoming" = due > today
            and not (status == .done or status == .cancelled) (icon: .calendar)
        filter "Todos" = status == .todo or status == .backlog (icon: .circle)
        filter "Done" = status == .done or status == .cancelled (icon: .circle_check)
        filter "Assigned to me" = assignee == invoker
            and not (status == .done or status == .cancelled) (icon: .user)
        filter "Nobody yet" = assignee == .none
            and contact == .none
            and not (status == .done or status == .cancelled) (icon: .circle_dashed)
        filter "Agent work" = assigned_by != .none
            and not (status == .done or status == .cancelled) (icon: .bot)
        filter "Waiting" = waiting_for != ""
            and not (status == .done or status == .cancelled) (icon: .hourglass)
        filter due
        filter project
        filter priority
        filter parent
        filter labels
        filter status
        filter assignee
        hide at
        hide remind
        hide assigned_by
        hide assigned_at
        hide estimate
        hide origin
        hide url
        hide work
        hide waiting_until
        hide waiting_on
        hide contact
        hide related
    }
}

/// One task another relates to, and how they relate: `blocks`, `blocked by`, `duplicate of`,
/// and so on.
type Related {
    /// The task it relates to.
    task: Task
    /// How they relate, in the service's words where it came from one.
    how: Text
}

/// The workspace's tasks and projects.
type Tasks {
    /// Every task, in the order people put them.
    items: Collection<Task>
    projects: Collection<Project>
    /// Where the tasks and threads here were first written: one for each place they came from.
    origins: Collection<Origin>
    /// The highest number given so far: the next task's is one more.
    numbers: Int merge counter
}

root tasks: Tasks

default view home for Tasks

default view task_page for Task

default view project_list for Project

views home (icon: .list), board (icon: .columns_2) for Tasks

views project_list (icon: .list), project_board (icon: .columns_2), project_page (icon: .info) for Project

row view task_summary for Task

place view to_do for Tasks

/// Adds a task to the inbox or to `p` at `status`: the entry at a list's end fills `title` with
/// the words nothing else there takes, and the day, moment and person they name, so "Call the
/// landlord fri 10am @Bo" is a task due Friday at ten, for Bo.
primary action new_task(
    tasks: Tasks,
    p: Option<Project> = .none,
    status: Option<Status> = .none,
    due: Option<Date> = .none,
    at: Option<Time> = .none,
    assignee: Option<Actor> = .none,
    title: Text = "",
    remind: Option<Time> = .none,
    notes: RichText = RichText(""),
) -> Task (icon: .plus, label: "New task", key: "Ctrl+N") {
    tasks.numbers += 1
    tasks.items.insert(
        Task(
            number: tasks.numbers,
            title: title,
            notes: notes,
            due: due,
            at: at,
            remind: remind,
            assignee: assignee,
            project: p,
            status: status ?? .backlog,
            done: closed(status ?? .backlog),
        ),
    )
}

/// Adds every task of `titles` at once, as one change: none of them is made when one cannot be
/// (a blank title), so a person's batch is whole or nothing.
action new_tasks(titles: List<Text>, p: Option<Project> = .none) -> List<Task> (
    icon: .plus,
    label: "New tasks",
    did: "added tasks",
) {
    require not titles.any(title -> title.trim().is_empty()) "A task needs a title"
    let made: List<Task> = []
    for title in titles {
        tasks.numbers += 1
        let one = tasks.items.insert(Task(number: tasks.numbers, title: title.trim(), project: p))
        made = [made, [one]].flat_map(x -> x)
    }
    made
}

/// Starts a project, with what it is for when the caller says.
action new_project(tasks: Tasks, notes: RichText = RichText("")) -> Project (
    icon: .folder_plus,
    label: "New project",
) {
    tasks.projects.insert(Project(name: std.say("New project"), notes: notes))
}

/// Sets whether a project is one with an end, or an ongoing responsibility.
action set_kind(p: Project, kind: Kind) (label: "Kind") {
    require p.kind != kind "It has that kind already"
    p.kind = kind
}

/// Renames a project.
action rename(p: Project, name: Text) {
    p.name = name
}

/// Adds someone involved in a project, with the role they have there.
action involve(p: Project, name: Text, role: Text = "") (icon: .user_plus, label: "Add person") {
    require not name.trim().is_empty() "Name the person"
    p.people.insert(Person(name: name.trim(), role: role))
}

/// Takes someone off a project.
action remove_person(p: Project, person: Person) (icon: .user_minus, label: "Remove person") {
    p.people.remove(person)
}

/// Deletes a project; its tasks go back to the inbox.
destructive action remove_project(p: Project) (icon: .trash, label: "Delete project") {
    for t in tasks.items.filter(t -> t.project == p) {
        t.project = .none
    }
    tasks.projects.remove(p)
}

/// Adds a task to a project at `status`. It is the project's primary action, so its key brings the
/// person to the entry that adds there.
primary action add_to(p: Project, title: Text, status: Option<Status> = .none) -> Task (
    icon: .plus,
    label: "New task",
    key: "Ctrl+N",
) {
    tasks.numbers += 1
    tasks.items.insert(
        Task(
            number: tasks.numbers,
            title: title,
            project: p,
            status: status ?? .backlog,
            done: closed(status ?? .backlog),
        ),
    )
}

/// Sets what a task is called.
action retitle(t: Task, title: Text) (icon: .pencil, label: "Rename") {
    require not title.trim().is_empty() "A task needs a title"
    t.title = title
}

/// Whether `place` is `top` or one of the tasks over it.
fn within(items: List<Task>, place: Option<Task>, top: Task) -> Bool {
    if let p = place {
        p == top or items.ancestors(p, .parent).any(a -> a == top)
    } else { false }
}

/// Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for
/// none), or where `before` stands. What is put under a closed task opens it.
action move_to(moved: Task, parent: Option<Task>, before: Option<Task>) uses time {
    require not within(tasks.items, parent, moved)
        and not within(tasks.items, before?.parent, moved) "A task cannot go inside itself"
    if let b = before {
        tasks.items.place(moved, .parent, before: b)
    } else {
        tasks.items.place(moved, .parent, inside: parent)
    }
    if not closed(moved.status) and tasks.items.ancestors(moved, .parent).any(a -> closed(a.status)) {
        set_status(moved, .todo)
    }
}

/// Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does; with
/// none named, under the task above it in the list.
action indent(t: Task, above: Option<Task> (above)) uses time (
    label: "Indent",
    icon: .arrow_right,
    key: "Ctrl+]",
    group: "Move",
) {
    applies above != .none and above != t.parent "Nothing over it to move under"
    tasks.items.place(t, .parent, under: above)
    if not closed(t.status) and tasks.items.ancestors(t, .parent).any(a -> closed(a.status)) {
        set_status(t, .todo)
    }
}

/// Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline.
action outdent(t: Task) (label: "Outdent", icon: .arrow_left, key: "Ctrl+[", group: "Move") {
    applies t.parent != .none "It is at the top level already"
    if let p = t.parent { tasks.items.place(t, .parent, after: p) }
}

/// Puts a task in a column of the board, at the place `at` among the tasks already there: what a
/// card's drag names.
action set_status(t: Task, status: Status, at: Int = 0) uses time (label: "Status", did: "moved") {
    require t.status != status "It stands there already"
    t.status = status
    t.done = closed(status)
    tasks.items.move(t, to: at, among: tasks.items.filter(x -> x.status == status and x != t))
    if closed(status) {
        // A column of finished work opens nothing: the task goes on as over it.
        for x in tasks.items.descendants(t, .parent) {
            if not closed(x.status) {
                x.status = status
                x.done = true
            }
        }
        for a in tasks.items.ancestors(t, .parent) {
            if closed(a.status) {
                a.status = .todo
                a.done = false
            }
        }
    } else if tasks.items.ancestors(t, .parent).any(a -> closed(a.status)) {
        for a in tasks.items.ancestors(t, .parent) {
            if closed(a.status) {
                a.status = .todo
                a.done = false
            }
        }
    }
    if closed(status) {
        if let p = t.parent {
            if p.waiting_for != "" and pieces_through(p) { resume(p, "the pieces are through") }
        }
    }
}

/// Whether the working time a task may use is spent.
fn time_used(t: Task) -> Bool {
    if let w = t.work {
        if let budget = w.budget_ms { w.worked_ms >= budget } else { false }
    } else { false }
}

/// Whether the money a task may spend is spent.
fn money_used(t: Task) -> Bool {
    if let w = t.work {
        if let budget = w.budget_micros { w.spent_micros >= budget } else { false }
    } else { false }
}

/// Whether the task's sessions were lost too often in a row to begin another: two ended without
/// finishing since the last one ended or its person said go on.
fn lost_too_often(t: Task) -> Bool {
    if let w = t.work { w.lost >= 2 } else { false }
}

/// Whether work on a piece is over for now: it is closed, waiting for a person to look, or its
/// work stopped.
fn through(x: Task) -> Bool {
    closed(x.status) or x.status == .in_review or x.work?.stopped != .none
}

/// Whether the work on a piece began and is not over.
fn in_work(x: Task) -> Bool {
    not through(x) and if let w = x.work { w.sessions > 0 } else { false }
}

/// Whether every task under `t` is through.
fn pieces_through(t: Task) -> Bool {
    let under = tasks.items.descendants(t, .parent)
    not under.is_empty() and under.all(x -> through(x))
}

/// Whether the task's pieces already fill the number of them its task lets be in work at once.
fn held_back(t: Task) -> Bool {
    if let p = t.parent {
        if let at_once = p.work?.pieces_at_once {
            tasks.items.filter(x -> x.parent == p and x != t).count(x -> in_work(x)) >= at_once
        } else { false }
    } else { false }
}

/// Puts every chosen task in one column (or project) at once, as one change.
action set_status_many(chosen: List<Task>, status: Status, at: Int = 0) (
    icon: .columns_2,
    label: "Move to",
    did: "moved tasks",
) {
    require not chosen.is_empty() "Choose the tasks to move"
    for t in chosen {
        if not closed(t.status) {
            t.status = status
            t.done = closed(status)
        }
    }
}

/// Checks a task off with every task under it, or opens it again with the tasks it is under.
action check(t: Task, done: Bool = not closed(t.status)) (
    label: if done { "Complete" } else { "Reopen" },
    icon: .check,
    on: closed(t.status),
    key: "Ctrl+Enter",
) {
    if closed(t.status) != done {
        set_status(t, if done { .done } else { .todo })
    }
}

/// Adds a subtask under a task, in its project and column, and opens a closed one: there is work
/// under it now.
action add_subtask(t: Task, title: Text) -> Task uses time (
    label: "Add subtask",
    icon: .list_tree,
    did: "added a subtask",
    key: "Ctrl+Shift+N",
) {
    tasks.numbers += 1
    let made = tasks.items.insert(
        Task(
            number: tasks.numbers,
            title: title,
            parent: t,
            project: t.project,
            status: t.status,
            done: closed(t.status),
        ),
    )
    let last = tasks.items.filter(x -> x.parent == t and x != made).last() ?? t
    tasks.items.move(made, after: last)
    if closed(t.status) { set_status(t, .todo) }
    made
}

/// Takes away the subtask the entry left empty: a field that began one and was left blank is no
/// step of the work, so nothing empty stands under the task.
action remove_empty_step(t: Task) (did: "removed an empty step") {
    for x in tasks.items.filter(x -> x.parent == t and x.title.trim().is_empty()) {
        tasks.items.remove(x)
    }
}

/// Sets the day a task is due by, from the presets or a day the person picks.
action due_today(t: Task) uses time (label: "Today", icon: .star, key: "Ctrl+T", group: "Due") {
    require not closed(t.status) "It is done already"
    t.at = moved(t.at, t.due, today)
    t.remind = moved(t.remind, t.due, today)
    t.due = today
}

/// Makes a task due tomorrow.
action due_tomorrow(t: Task) uses time (label: "Tomorrow", icon: .calendar, group: "Due") {
    require not closed(t.status) "It is done already"
    t.at = moved(t.at, t.due, today + 1d)
    t.remind = moved(t.remind, t.due, today + 1d)
    t.due = today + 1d
}

/// Makes a task due at the start of next week.
action due_next_week(t: Task) uses time (label: "Next week", icon: .calendar, group: "Due") {
    require not closed(t.status) "It is done already"
    t.at = moved(t.at, t.due, today.start_of_week() + 1w)
    t.remind = moved(t.remind, t.due, today.start_of_week() + 1w)
    t.due = today.start_of_week() + 1w
}

/// Makes a task due on a day the person picks. Its moment and its reminder move with it, at the
/// same time of day: a task due Wednesday at six, reminded at six, moved to Friday is due Friday
/// at six and reminded then.
action schedule(t: Task, due: Option<Date>) (
    label: "Pick date",
    icon: .calendar,
    key: "Ctrl+D",
    group: "Due",
) {
    t.at = moved(t.at, t.due, due)
    t.remind = moved(t.remind, t.due, due)
    t.due = due
}

/// `moment` moved with its task from the day `from` to the day `to`, at the same time of day; as
/// it was when either day is missing. Each action that moves a task's day writes it itself, so a
/// command over many chosen tasks stays one change.
fn moved(moment: Option<Time>, from: Option<Date>, to: Option<Date>) -> Option<Time> {
    if let m = moment {
        if let f = from { if let d = to { m + (d - f) } else { m } } else { m }
    }
}

/// Sets the time of day a task is due at: the day it is on stays, and a task with no day is due
/// on the day the moment names. A reminder that was set for the old moment moves with it, as a
/// day moved by `schedule` keeps its time of day.
action set_time(t: Task, at: Option<Time>) uses time (
    label: "Set time",
    icon: .clock,
    group: "Due",
) {
    let when: Option<Time> = if let m = at {
        if let d = t.due { .some(m + (d - m.day())) } else { .some(m) }
    } else { .none }
    if let old = t.at {
        if t.remind == .some(old) {
            t.remind = when
        }
    }
    t.at = when
    if t.due == .none {
        if let w = when {
            t.due = w.day()
        }
    }
}

/// Clears the day a task is due by: it waits in the inbox, undated.
action due_clear(t: Task) (label: "No date", group: "Due") {
    require t.due != .none "It has no date"
    t.due = .none
}

/// Rings a task's reminder when its moment comes, once for the whole workspace, window or not.
trigger ring(t: Task) at t.remind: remind(t)

/// Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task
/// is due moves the task to that day, its moment with it.
action remind_at(t: Task, when: Time) uses time (label: "Remind me", icon: .bell, group: "Remind") {
    if let d = t.due {
        if when.day() > d {
            t.at = moved(t.at, t.due, when.day())
            t.due = when.day()
        }
    }
    t.remind = when
}

/// Clears a task's reminder: it is not told of again.
action remind_clear(t: Task) uses time (label: "No reminder", icon: .bell_off, group: "Remind") {
    require t.remind != .none "It has no reminder"
    t.remind = .none
}

/// Tells whoever is doing a task, or the one who made it, what it is and when it is due: once,
/// and not of a task already closed. Choosing the notice opens the task.
action remind(t: Task) uses notify {
    t.remind = .none
    if closed(t.status) {
        return
    }
    let who: Option<Actor> = if let a = t.assignee { a } else if let made = created(t) { made.who }
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

/// Sets the points a task is worth, or none.
action estimate(t: Task, points: Option<Int>) (label: "Estimate", icon: .gauge) {
    t.estimate = points
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

/// Gives a task to someone, or to nobody: a member of the workspace by their actor, or someone
/// the workspace keeps in touch with by their contact. Who gave it to an agent, and when, stay
/// on the task: the assignment is the change the agent's own turn is triggered by.
action assign(
    t: Task,
    who: Option<Actor> = .none,
    contact: Option<contacts.Contact> = .none,
) uses time (label: "Assign", icon: .user) {
    require not (who != .none and contact != .none) "A task is a member's or a contact's, not both"
    require not (who?.kind() == .agent and held_back(t)) "The pieces in work already fill what this task lets work at once"
    require not (t.assignee == who and t.contact == contact) "It is theirs already"
    t.assignee = who
    t.contact = contact
    let to_agent = if let w = who { w.kind() == .agent } else { false }
    t.assigned_by = if to_agent { .some(invoker) } else { .none }
    t.assigned_at = if to_agent { .some(now) } else { .none }
}

/// Gives every chosen task to one person (or nobody), as one change.
action assign_many(chosen: List<Task>, who: Option<Actor>) uses time (
    label: "Assign",
    icon: .user,
    did: "assigned tasks",
) {
    require not chosen.is_empty() "Choose the tasks to assign"
    for t in chosen {
        if t.assignee != who or t.contact != .none {
            t.assignee = who
            t.contact = .none
            let to_agent = if let w = who { w.kind() == .agent } else { false }
            t.assigned_by = if to_agent { .some(invoker) } else { .none }
            t.assigned_at = if to_agent { .some(now) } else { .none }
        }
    }
}

/// Links a task to another, with how they relate: the workspace's own link between two tasks,
/// which the page of the task it relates to draws too.
action relate(t: Task, other: Task, how: Text) (
    icon: .link,
    label: "Relate to…",
    did: "linked a task",
) {
    require t != other "A task does not relate to itself"
    require not t.related.any(r -> r.task == other and r.how == how) "They are related already"
    t.related.insert(Related(task: other, how: how))
}

/// Takes a relation off a task.
destructive action unrelate(t: Task, relation: Related) (icon: .x, label: "Remove relation") {
    require t.related.any(r -> r == relation) "It has no such relation"
    t.related.remove(relation)
}

/// Gives a task a label.
action label(t: Task, name: Text) (icon: .tag, label: "Label…", group: "Labels") {
    require not name.trim().is_empty() "Name the label"
    t.labels.add(name.trim())
}

/// Takes a label off a task.
action unlabel(t: Task, name: Text) (label: "Remove label", group: "Labels") {
    require t.labels.contains(name) "It has no such label"
    t.labels.remove(name)
}

/// Puts every chosen task under one label, as one change.
action label_many(chosen: List<Task>, name: Text) (
    icon: .tag,
    label: "Label…",
    did: "labelled tasks",
) {
    require not name.trim().is_empty() "Name the label"
    for t in chosen {
        t.labels.add(name.trim())
    }
}

/// Attaches a file to a task.
action attach(t: Task, file: File) (
    icon: .paperclip,
    label: "Attach file",
    did: "attached a file",
) {
    t.attachments = [t.attachments, [file]].flat_map(x -> x)
}

/// Takes an attachment off a task.
action detach(t: Task, attachment: File) (icon: .x, label: "Remove attachment") {
    t.attachments = t.attachments.filter(a -> a.name != attachment.name)
}

/// Opens the attachment `attachment` in the app that opens its kind: the file a task holds
/// reaches that app as the same content both read, never as a path neither has.
action open_attachment(t: Task, attachment: File) uses files (
    icon: .external_link,
    label: "Open attachment",
) {
    match File.open(attachment) {
        .ok => {}
        .err(_) => {}
    }
}

/// Imports files as tasks: one task for each file, under the project `p` when one is named, the
/// file attached to it. A file of the same name already carried by a task is not imported twice.
action import_files(files: List<File>, p: Option<Project> = .none) -> List<Task> (
    icon: .file_plus,
    label: "Import files",
    did: "imported files",
) {
    require not files.is_empty() "Choose the files to import"
    let made: List<Task> = []
    for file in files {
        let known = tasks.items.any(
            t -> t.attachments.any(a -> a.url == file.url or a.name == file.name),
        )
        if not known {
            tasks.numbers += 1
            let task = tasks.items.insert(Task(number: tasks.numbers, title: file.name, project: p))
            task.attachments = [task.attachments, [file]].flat_map(x -> x)
            made = [made, [task]].flat_map(x -> x)
        }
    }
    made
}

/// Starts a task's work on another tracker's issue: its origin, its page there and its number,
/// and a task of the same origin already here is that task, so importing it again makes none.
/// Names where a task imported here was first written: one origin per place it came from.
action add_origin(
    tasks: Tasks,
    author: Text,
    source: Text,
    handle: Text = "",
    url: Text = "",
    reference: Text = "",
    created: Option<Time> = .none,
    updated: Option<Time> = .none,
) -> Origin (icon: .link, label: "Add origin", did: "named where it came from") {
    tasks.origins.insert(
        Origin(
            author: author,
            handle: if handle == "" { .none } else { .some(handle) },
            source: source,
            url: if url == "" { .none } else { .some(url) },
            reference: if reference == "" { .none } else { .some(reference) },
            created: created,
            updated: updated,
        ),
    )
}

action import_issue(origin: Origin, title: Text, key: Text, status: Status = .backlog) -> Task (
    icon: .download,
    label: "Import issue",
    did: "imported an issue",
) {
    require not key.trim().is_empty() "An imported issue needs a key"
    if let found = tasks.items.find(
            t -> t.origin?.source == origin.source and t.origin?.reference == .some(key),
        ) {
        found
    } else {
        tasks.numbers += 1
        tasks.items.insert(
            Task(
                number: tasks.numbers,
                title: title,
                status: status,
                done: closed(status),
                origin: .some(origin),
                url: origin.url,
            ),
        )
    }
}

/// Writes a task's note in Notes: a note named like the task, holding what it remembers, made
/// from the task so it reads under it.
action write_note(t: Task) -> notes.Note (
    icon: .file_text,
    label: "Write its note",
    did: "wrote a note",
) {
    notes.new_note(notes.notebook, title: t.title, body: t.notes.markdown).made_from(t)
}

/// Puts a task on hold until what it waits for comes, or until `until` at the latest: whoever is
/// doing it goes on with it then, a person or an agent alike. `on` names the automation that ends
/// the wait early when it runs.
action wait(
    t: Task,
    what: Text,
    until: Option<Time> = .none,
    kind: WaitKind = .reply,
    on: Text = "",
) uses time (label: "Wait for…", icon: .hourglass, did: "put on hold", group: "Plan") {
    require not closed(t.status) "It is done already"
    require not what.trim().is_empty() "Say what it waits for"
    require to_come(until, now) "The wait ends in the past"
    let one = Wait(kind: kind, what: what.trim(), until: until, event: on)
    let waits: List<Wait> = if let w = t.work { [w.waits, [one]].flat_map(x -> x) } else { [one] }
    t.work = if let w = t.work {
        Work(
            folder: w.folder,
            note: w.note,
            waits: [w.waits, [one]].flat_map(x -> x),
            until: latest(until, w.until),
            since: w.since ?? now,
            budget_micros: w.budget_micros,
            budget_ms: w.budget_ms,
            budget_by: w.budget_by,
            spent_micros: w.spent_micros,
            worked_ms: w.worked_ms,
            sessions: w.sessions,
            session_since: w.session_since,
            lost: w.lost,
            stopped: w.stopped,
            resumed: w.resumed,
            pieces_at_once: w.pieces_at_once,
        )
    } else {
        Work(waits: waits, until: until, since: .some(now))
    }
    t.waiting_for = what.trim()
    t.waiting_until = latest(until, t.work?.until)
    t.waiting_on = on
}

/// Whether the moment `until` is still to come at `at`, or none was named.
fn to_come(until: Option<Time>, at: Time) -> Bool {
    if let u = until { u >= at } else { true }
}

/// The later of two moments, either of which may be missing.
fn latest(a: Option<Time>, b: Option<Time>) -> Option<Time> {
    if let x = a {
        if let y = b { if y > x { .some(y) } else { .some(x) } } else { .some(x) }
    } else { b }
}

/// Ends every wait a task is on: what it waited for came, which `cause` says in a few words, and
/// whoever is doing it goes on with it. It is also how a person starts its work over after its
/// sessions were lost too often: the losses no longer stand in a row, and a session may begin.
action resume(t: Task, cause: Text = "") uses time (
    label: "Stop waiting",
    icon: .play,
    did: "went on with",
    group: "Plan",
) {
    applies t.waiting_for != "" or t.waiting_until != .none or lost_too_often(t) "It waits for nothing, and none of its sessions was lost"
    if let w = t.work {
        t.work = Work(
            folder: w.folder,
            note: w.note,
            waits: [],
            until: .none,
            since: .none,
            budget_micros: w.budget_micros,
            budget_ms: w.budget_ms,
            budget_by: w.budget_by,
            spent_micros: w.spent_micros,
            worked_ms: w.worked_ms,
            sessions: w.sessions,
            session_since: w.session_since,
            lost: 0,
            stopped: w.stopped,
            resumed: .some(Resumed(cause: cause, at: now)),
            pieces_at_once: w.pieces_at_once,
        )
        // Its person said go on: the sessions lost before that no longer stand in a row.
    }
    t.waiting_for = ""
    t.waiting_until = .none
    t.waiting_on = ""
}

/// Ends a task's wait when its moment comes, once for the whole workspace, window or not.
trigger wait_ends(t: Task) at t.waiting_until: resume(t)

/// Sets where a task's working material is: the folder of its work, and the note its sessions
/// write.
action set_place(t: Task, folder: Text = "", note: Text = "") (
    label: "Working place",
    icon: .folder_open,
    did: "set the working place",
) {
    let w = work_of(t)
    t.work = Work(
        folder: folder,
        note: note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: w.budget_micros,
        budget_ms: w.budget_ms,
        budget_by: w.budget_by,
        spent_micros: w.spent_micros,
        worked_ms: w.worked_ms,
        sessions: w.sessions,
        session_since: w.session_since,
        lost: w.lost,
        stopped: w.stopped,
        resumed: w.resumed,
        pieces_at_once: w.pieces_at_once,
    )
}

/// Sets what a task's work may spend and how long it may work, and whose budget it charges.
action set_budget(
    t: Task,
    micros: Option<Int> = .none,
    millis: Option<Int> = .none,
    by: BudgetBy = .person,
) (label: "Budget", icon: .gauge, did: "set a budget") {
    let w = work_of(t)
    t.work = Work(
        folder: w.folder,
        note: w.note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: micros,
        budget_ms: millis,
        budget_by: by,
        spent_micros: w.spent_micros,
        worked_ms: w.worked_ms,
        sessions: w.sessions,
        session_since: w.session_since,
        lost: w.lost,
        stopped: w.stopped,
        resumed: w.resumed,
        pieces_at_once: w.pieces_at_once,
    )
}

/// How many of a task's pieces may be in work at once.
action set_pieces_at_once(t: Task, pieces: Option<Int>) (
    label: "Pieces at once",
    icon: .git_branch,
    did: "set the pieces at once",
) {
    let w = work_of(t)
    t.work = Work(
        folder: w.folder,
        note: w.note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: w.budget_micros,
        budget_ms: w.budget_ms,
        budget_by: w.budget_by,
        spent_micros: w.spent_micros,
        worked_ms: w.worked_ms,
        sessions: w.sessions,
        session_since: w.session_since,
        lost: w.lost,
        stopped: w.stopped,
        resumed: w.resumed,
        pieces_at_once: pieces,
    )
}

/// A task's work as it stands, or as it starts when nothing worked it yet.
fn work_of(t: Task) -> Work {
    if let w = t.work { w } else { Work(waits: []) }
}

/// Says a session on a task began: counted, and its start kept for how long it works.
action session_began(t: Task) uses time (label: "Session began", did: "began work") {
    require not time_used(t) "I stopped: the working time this task was given is used up. Give it more time to let me go on."
    require not money_used(t) "I stopped: this task's budget is spent. Raise it to let me go on."
    require not lost_too_often(t) "I stopped: my work on this was interrupted too often in a row. Tell me to go on and I start again from its notes."
    let w = work_of(t)
    t.work = Work(
        folder: w.folder,
        note: w.note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: w.budget_micros,
        budget_ms: w.budget_ms,
        budget_by: w.budget_by,
        spent_micros: w.spent_micros,
        worked_ms: w.worked_ms,
        sessions: w.sessions + 1,
        session_since: .some(now),
        lost: w.lost,
        stopped: w.stopped,
        resumed: .none,
        pieces_at_once: w.pieces_at_once,
    )
    t.status = .in_progress
    t.done = false
}

/// Says a session on a task ended: what it spent and how long it worked are added to the task.
action session_ended(t: Task, spent_micros: Int = 0, worked_ms: Int = 0) uses time (
    label: "Session ended",
    did: "ended work",
) {
    let w = work_of(t)
    t.work = Work(
        folder: w.folder,
        note: w.note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: w.budget_micros,
        budget_ms: w.budget_ms,
        budget_by: w.budget_by,
        spent_micros: w.spent_micros + spent_micros,
        worked_ms: w.worked_ms + worked_ms,
        sessions: w.sessions,
        session_since: .none,
        lost: 0,
        stopped: w.stopped,
        resumed: w.resumed,
        pieces_at_once: w.pieces_at_once,
    )
    // A session that ended finished its work: what was lost before it no longer stands in a
    // row with anything.
}

/// Says a task's work stopped, and why, in the words its person reads.
action stop(t: Task, why: StoppedWhy, words: Text = "") uses time (
    label: "Stop",
    icon: .square,
    did: "stopped work",
) {
    let w = work_of(t)
    t.work = Work(
        folder: w.folder,
        note: w.note,
        waits: w.waits,
        until: w.until,
        since: w.since,
        budget_micros: w.budget_micros,
        budget_ms: w.budget_ms,
        budget_by: w.budget_by,
        spent_micros: w.spent_micros,
        worked_ms: w.worked_ms,
        sessions: w.sessions,
        session_since: .none,
        lost: w.lost + 1,
        stopped: .some(Stopped(why: why, words: words, at: now)),
        resumed: w.resumed,
        pieces_at_once: w.pieces_at_once,
    )
}

/// Copies a task, with every task under it, just after the one it copies.
action duplicate(t: Task) -> Task (label: "Duplicate", icon: .copy, key: "Ctrl+Shift+D") {
    let under = tasks.items.descendants(t, .parent)
    tasks.numbers += 1
    let copy = tasks.items.insert(copied(t, t.parent))
    share_with(copy, t)
    if let last = last_below(tasks.items, t) {
        tasks.items.move(copy, after: last)
    } else { tasks.items.move(copy, after: t) }
    // Each copy hangs under the copy of the task it stands under, at the place it takes itself.
    for x in under {
        let under_copy = under.index_of(x.parent ?? t) ?? -1
        let at = tasks.items.descendants(copy, .parent).at(under_copy) ?? copy
        let below = last_below(tasks.items, at) ?? at
        tasks.numbers += 1
        let made = tasks.items.insert(copied(x, at))
        share_with(made, x)
        tasks.items.move(made, after: below)
    }
    copy
}

/// Deletes a task and its subtasks.
destructive action remove(t: Task) (label: "Delete", icon: .trash, key: "Delete") {
    for x in tasks.items.descendants(t, .parent) {
        tasks.items.remove(x)
    }
    tasks.items.remove(t)
}

/// Deletes every finished task and answers how many went: a finished branch goes whole, and a
/// done task with a step still open under it stays.
destructive action clear_done(tasks: Tasks) -> Int (
    icon: .circle_check,
    label: "Delete finished tasks",
) {
    require tasks.items.count(x -> closed(x.status)) > 0 "Nothing is finished yet"
    let every = tasks.items.filter(x -> true)
    let gone = every.filter(x -> finished(every, x))
    for x in gone {
        tasks.items.remove(x)
    }
    gone.len()
}

/// `t` as a task of its own under `parent`: what a copy of it says, without the tasks under it
/// and its labels and files, which the copy is given once it is made.
fn copied(t: Task, parent: Option<Task>) -> Task {
    let copy = Task(
        number: tasks.numbers,
        title: t.title,
        notes: t.notes,
        status: t.status,
        done: t.done,
        due: t.due,
        at: t.at,
        remind: t.remind,
        assignee: t.assignee,
        contact: t.contact,
        assigned_by: t.assigned_by,
        assigned_at: t.assigned_at,
        project: t.project,
        priority: t.priority,
        estimate: t.estimate,
        parent: parent,
        origin: t.origin,
        url: t.url,
        work: t.work,
        waiting_for: t.waiting_for,
        waiting_until: t.waiting_until,
        waiting_on: t.waiting_on,
    )
    copy
}

/// Gives the copy `to` what `from` holds in its set, its list and its collection: what only an
/// action may change.
action share_with(to: Task, from: Task) (did: "copied a task") {
    for l in from.labels {
        to.labels.add(l)
    }
    for a in from.attachments {
        to.attachments = [to.attachments, [a]].flat_map(x -> x)
    }
    for r in from.related {
        to.related.insert(r)
    }
}

/// The task that ends the run `t` begins: the last task under `t`, or `t` itself.
fn last_below(items: List<Task>, t: Task) -> Option<Task> {
    items.filter(x -> x == t or items.ancestors(x, .parent).any(a -> a == t)).last()
}

/// Whether `t` is finished: closed itself, and every task under it, read over the caller's list.
fn finished(items: List<Task>, t: Task) -> Bool {
    let under = items.descendants(t, .parent)
    if under.is_empty() { closed(t.status) } else { under.all(x -> closed(x.status)) }
}

/// Whether `t` stands in a list's Done tail: finished, and everything over it too.
fn folded(items: List<Task>, t: Task) -> Bool {
    finished(items, t) and items.ancestors(t, .parent).all(a -> finished(items, a))
}

/// How many of a task's subtasks are done ("1/3"), wherever they stand.
fn subtask_count(t: Task) -> Option<Text> {
    let mine = tasks.items.filter(x -> x.parent == t)
    if let total = mine.counted() {
        if not mine.is_empty() {
            "{mine.count(x -> closed(x.status))}/{total}"
        }
    }
}

/// When a task is due of its own: a subtask repeats no day its parent already says.
fn own_due(t: Task, under: Option<Task>) -> Option<Date> {
    if let p = under { if p.due != t.due { t.due } } else { t.due }
}

/// When a task was finished: when its status last changed, from the index, never its history.
fn finished_at(t: Task) -> Option<Time> {
    if let v = changed(t, .status) { if closed(t.status) { v.at } }
}

/// The day a task stands under in a place's list: its due day, or in Done the day it was finished.
fn task_day(t: Task, by_day: Bool, done_place: Bool) -> Option<Date> {
    if by_day { t.due } else if done_place { finished_at(t)?.day() }
}

/// How many wait somewhere, as a badge shows it: none while nothing waits, or before all is read.
fn waiting(count: Option<Int>) -> Option<Int> {
    if let n = count { if n > 0 { count } }
}

/// The place `key` names among the type's own filters.
fn place_named(every: List<Filter<Task>>, key: Text) -> Filter<Task> {
    every.find(f -> f.key == key) ?? Task.inbox
}

/// The places a window offers: what is assigned to me is one only where someone else may share it.
fn places(shared: Bool) -> List<Filter<Task>> {
    let every = filters(Task)
    let out = [
        place_named(every, "all_issues"),
        place_named(every, "inbox"),
        place_named(every, "today"),
        place_named(every, "upcoming"),
        place_named(every, "todos"),
        place_named(every, "nobody_yet"),
        place_named(every, "agent_work"),
        place_named(every, "waiting"),
        place_named(every, "done"),
    ]
    if shared {
        [out, [place_named(every, "assigned_to_me")]].flat_map(x -> x)
    } else { out }
}

/// The place a task stands in: its project's, or the first of `side` that holds it, else Inbox.
fn place_of(side: List<Filter<Task>>, x: Task) -> Filter<Task> {
    if let p = x.project {
        Task.project(p)
    } else {
        side.find(f -> tasks.items.matching(f).any(t -> t == x)) ?? Task.inbox
    }
}

/// The labels in use across the workspace's tasks, once each, in order.
fn labels_in_use(items: List<Task>) -> List<Text> {
    let seen: Map<Bool> = Map<Bool>()
    let out: List<Text> = []
    for t in items {
        for l in t.labels.sort_by(x -> x) {
            if seen.get(l) == .none {
                seen.put(l, true)
                out = [out, [l]].flat_map(x -> x)
            }
        }
    }
    out
}

/// The tasks of a column of a board: the open ones of `status`, never a card of a subtask whose
/// parent is in the same column.
fn column(p: Option<Project>, status: Status) -> List<Task> {
    tasks.items.filter(
        t -> t.status == status and (p == .none or t.project == p) and t.parent == .none,
    )
}

/// The tasks window: every issue, its places, its projects and its labels.
view home(t: Tasks) {
    state chosen: Text = "today"
    state opened: Option<Task> = .none
    // What the bar's search holds: it narrows the chosen place's tasks.
    state searched: Text = ""
    on show(x: Task): set(
            chosen,
            if let p = x.project {
                "project:{p.name}"
            } else {
                place_of(places(workspace.members.people.len() > 1), x).key
            },
        )
        .then(set(opened, x))
    let side = places(workspace.members.people.len() > 1)
    let project = t.projects.find(p -> chosen == "project:{p.name}")
    let label = if chosen.starts_with("label:") { chosen.slice(6, chosen.len()) } else { "" }
    let place = side.find(f -> f.key == chosen) ?? Task.today
    let listed = if let p = project {
        t.items.filter(x -> x.project == p)
    } else if label != "" {
        t.items.filter(x -> x.labels.contains(label))
    } else { t.items.matching(place) }
    let found = if searched.is_empty() { listed } else { listed.matching(searched) }
    // A place of days groups by day and a project's list folds finished work under its foot.
    let by_day = place.key == "today" or place.key == "upcoming" or project != .none
    let done_place = place.key == "done" and project == .none
    let folds = project != .none
    let in_today = place.key == "today" and project == .none
    // Due soonest first, Done newest finished first, a project's list with finished work last.
    let ordered = found.sort_by(.due)
    let shown = if done_place {
        found.sort_by(x -> changed(x, .status)?.at, desc)
    } else if folds { ordered.sort_by(x -> folded(found, x)) } else { ordered }
    let title = if label != "" {
        label
    } else if let p = project {
        p.name
    } else if chosen == "all_issues" { "All issues" } else { place.label }
    // What waits here, read from the index: the same cost at three tasks or three thousand.
    let open = found.count(x -> not closed(x.status))
    let all_done = open == 0 and not found.is_empty()
    // The places whose badge counts what waits.
    let counted: List<Text> = [
        "all_issues",
        "inbox",
        "today",
        "todos",
        "nobody_yet",
        "agent_work",
        "waiting",
        "assigned_to_me",
    ]
    let header_count = if label == "" {
        if counted.any(k -> k == chosen) { waiting(found.counted()) }
    } else { found.counted() }
    let entry_placeholder = if label != "" {
        "New task labelled {label}"
    } else if let p = project {
        "New task in {p.name}"
    } else if in_today { "New task for today" } else { "New task" }
    let labels = labels_in_use(t.items)
    // The places, the tasks of one, and the opened task beside them: the list is the window's
    // overview, so the open task's page stands in the content column and the list keeps its
    // place left of it, rather than the page growing out of the row under the list's foot.
    ui.NavShell(
        content_kind: .document,
        sidebar_title: "Tasks",
        overview_title: title,
        overview_count: header_count,
        title: if let x = opened { meta(x).title } else { title },
        nothing_listed: t.items.is_empty(),
        actions: [new_task, clear_done],
    ) {
        on search(q: Text): set(searched, q)
        sidebar: {
            ui.NavList(selected: chosen) {
                for f in side {
                    let badge = if counted.any(k -> k == f.key) {
                        waiting(t.items.matching(f).counted())
                    }
                    ui.NavItem(f.key, f.label, icon: f.icon, count: badge) {
                        on select: set(chosen, f.key).then(set(opened, .none))
                    }
                }
                ui.NavSection("Projects", add: new_project(t)) {
                    for p in t.projects {
                        ui.NavItem(
                            "project:{p.name}",
                            p.name,
                            icon: if p.kind == .area { .layers } else { .folder },
                        ) {
                            on select: set(chosen, "project:{p.name}").then(set(opened, .none))
                        }
                    }
                }
                if not labels.is_empty() {
                    ui.NavSection("Labels") {
                        for l in labels {
                            ui.NavItem("label:{l}", l, icon: .tag) {
                                on select: set(chosen, "label:{l}").then(set(opened, .none))
                            }
                        }
                    }
                }
            }
        }
        overview: {
            ui.Docked {
                foot: {
                    stack {
                        if all_done {
                            task_list(shown, by_day, done_place, folds, opened, project)
                        }
                        std.add_entry(
                            entry_placeholder,
                            new_task(
                                t,
                                p: project,
                                status: if let pr = project {
                                    .some(status_of(place.key))
                                } else { .none },
                                due: if in_today { today },
                            ),
                        )
                    }
                }
                if all_done {
                    ui.EmptyState(phrase("tilolang-all-done"), icon: .list_checks, inline: true)
                } else if shown.is_empty() and opened == .none {
                    ui.EmptyState("No tasks here", icon: .list_checks)
                } else {
                    task_list(shown, by_day, done_place, folds, opened, project)
                }
            }
        }
        content: {
            // Nothing open: the list stands alone, as a window with one column does. Only an
            // empty place says something here, in place of the columns the shell would show.
            if let x = opened {
                std.document_page {
                    task_body(x)
                }
            } else if t.items.is_empty() {
                ui.EmptyState("No tasks here", icon: .list_checks)
            }
        }
    }
}

/// The column a place's new tasks start in.
fn status_of(key: Text) -> Status {
    if key == "done" {
        .done
    } else if key == "todos" { .todo } else if key == "waiting" { .backlog } else { .backlog }
}

/// Every task on a board of its own, by the column it stands in: what the Issues/Board views show
/// of the whole workspace.
view board(t: Tasks) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    std.page(t, canvas: true, fill: true) {
        ui.Scroll(across: true) {
            ui.Board(name: "Tasks") {
                for s in cases(Status) {
                    let cards = column(.none, s)
                    ui.BoardList(title: s.label(), count: cards.counted()) {
                        on drop(moved: Task, at: Int): set_status(moved, s, at)
                        foot: {
                            std.add_entry("New task", new_task(tasks, status: .some(s)))
                        }
                        for x in cards {
                            task_card(x, .none)
                        }
                    }
                }
            }
        }
    }
}

/// What there is to do, on its own as a place shows it: the open tasks, due soonest first and
/// grouped by day, each opening where it stands.
view to_do(t: Tasks) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    let open = t.items.filter(x -> not closed(x.status)).sort_by(.due)
    ui.Docked {
        foot: {
            stack {
                std.add_entry("New task", new_task(t))
            }
        }
        if open.is_empty() {
            ui.EmptyState("Nothing to do", icon: .list_checks)
        } else {
            task_list(open, true, false, false, opened, .none)
        }
    }
}

/// The tasks of a place as a list, branches nested under their tasks, finished work under the foot.
view task_list(
    items: List<Task>,
    by_day: Bool,
    done_place: Bool,
    folds: Bool,
    opened: State<Option<Task>>,
    project: Option<Project>,
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
        let above = if let p = items.previous(x) {
            if task_day(p, by_day, done_place) == heading and (folds and folded(items, p)) == tail {
                p
            }
        }
        task_item(x, at, project, heading, done_place, tail, above, subtask_count(x))
    }
}

/// One task as a row of a list, and the document that opens into it where it stands.
view task_item(
    t: Task,
    at: Option<Actor>,
    project: Option<Project>,
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
        done: t.done,
        tail: tail,
        above: above,
        parent: t.parent,
        parent_title: t.parent?.title,
        progress: progress,
        actions: [
            check(primary: true),
            schedule(group: "Plan"),
            due_today(group: "Plan"),
            due_tomorrow(group: "Plan"),
            due_next_week(group: "Plan"),
            due_clear(group: "Plan"),
            set_time(group: "Plan"),
            remind_at(group: "Plan"),
            remind_clear(group: "Plan"),
            set_priority_none(group: "Priority"),
            set_priority_low(group: "Priority"),
            set_priority_medium(group: "Priority"),
            set_priority_high(group: "Priority"),
            set_priority_urgent(group: "Priority"),
            assign(group: "People"),
            file(group: "Move"),
            unfile(group: "Move"),
            indent(group: "Move"),
            outdent(group: "Move"),
            add_subtask(group: "More"),
            duplicate(group: "More"),
            label(group: "More"),
            wait(group: "More"),
            resume(group: "More"),
            remove(group: "More"),
        ],
    ) {
        // The row leaves out the project the window is of, and the day its group is headed by.
        // It opens no page of its own: the task's page is the window's content column, beside
        // the list the row stands in, so the list never continues under it.
        task_row(t, project, group != .none, t.parent)
        menu: {
            ui.MenuItem("Open as page", icon: .arrow_up_right) {
                on choose: open(t, view: task_page)
            }
        }
    }
}

/// A project's window: its tasks, with the entry that adds to them at their foot.
view project_list(p: Project) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    let listed = tasks.items.filter(t -> t.project == p)
    ui.NavShell(content_kind: .list, title: p.name) {
        content: {
            ui.Docked {
                foot: {
                    stack {
                        std.add_entry("New task in {p.name}", new_task(tasks, p: p))
                    }
                }
                if listed.is_empty() {
                    ui.EmptyState("No tasks yet", icon: .folder)
                } else {
                    task_list(listed, false, false, false, opened, .some(p))
                }
            }
        }
    }
}

/// A project's board: a list per column of its statuses, each open task a card.
view project_board(p: Project) {
    std.page(p, canvas: true, fill: true) {
        ui.Scroll(across: true) {
            ui.Board(name: p.name) {
                for s in cases(Status) {
                    let cards = column(.some(p), s)
                    ui.BoardList(title: s.label(), count: cards.counted()) {
                        on drop(moved: Task, at: Int): set_status(moved, s, at)
                        foot: {
                            std.add_entry("New task", new_task(tasks, p: p, status: .some(s)))
                        }
                        for x in cards {
                            task_card(x, .some(p))
                        }
                    }
                }
            }
        }
    }
}

/// A task on a board: its row, opening in a window of its own, with its column moves in its menu.
view task_card(t: Task, p: Option<Project>) {
    ui.BoardCard(
        t,
        actions: [
            check(primary: true),
            schedule(group: "Plan"),
            due_today(group: "Plan"),
            due_tomorrow(group: "Plan"),
            due_clear(group: "Plan"),
            set_priority_low(group: "Priority"),
            set_priority_medium(group: "Priority"),
            set_priority_high(group: "Priority"),
            set_priority_urgent(group: "Priority"),
            set_priority_none(group: "Priority"),
            assign(group: "People"),
            add_subtask(group: "More"),
            duplicate(group: "More"),
            label(group: "More"),
            remove(group: "More"),
        ],
    ) {
        on activate: open(t)
        menu: {
            for s in cases(Status) {
                if t.status != s {
                    ui.MenuItem("Move to {s.label()}", icon: .arrow_right) {
                        on choose: set_status(t, s, 0)
                    }
                }
            }
        }
        task_row(t, p, false, .none)
    }
}

/// One task as a row, leaving out what the window and the row over it already say: the project
/// the window is of (`none` for a place that gathers the tasks of every project), and the day
/// the row's group is headed by.
/// Where a row says the task stands: the project it is in, when that is not the window's, and
/// who it is a contact's.
fn placed(t: Task, project: Option<Project>) -> Option<Text> {
    let place = if t.project != project { t.project?.name }
    if let c = t.contact {
        .some(if let p = place { "{p} · {c.name}" } else { c.name })
    } else { place }
}

view task_row(t: Task, project: Option<Project>, day: Bool, under: Option<Task>) {
    let due: Option<Date> = if day { .none } else { own_due(t, under) }
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: placed(t, project),
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
        secondary: placed(t, .none),
        meta: t.due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// One task in an outline, counting done subtasks while folded, naming only what differs.
view task_outline_row(t: Task, project: Option<Project>, under: Option<Task>) {
    ui.TreeViewRow(
        checkbox: t.done,
        icon: if t.status == .backlog { t.priority.icon() } else { t.status.icon() },
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
    // The task's head — what it is and what was written of it — and the groups under it: the
    // page's own blocks, a group gutter apart.
    stack(gap: .group) {
        stack(gap: .related) {
            row(gap: .related, align: .start) {
                button(check(t))
                ui.TextInput(bind: t.title, placeholder: "What to do", title: true, fill: 1.0)
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Text("#{t.number}", secondary: true)
                let from = if let o = t.origin {
                    if let r = o.reference { "From {o.source} {r}" } else { "From {o.source}" }
                } else { "" }
                if from != "" {
                    ui.Text(from, secondary: true)
                }
                if let u = t.url {
                    ui.Link(u, label: u)
                }
            }
            if t.waiting_for != "" {
                row(gap: .related, align: .center) {
                    ui.Icon(.hourglass, size: .small)
                    ui.Text(
                        if let u = t.waiting_until {
                            "Waiting for {t.waiting_for}, until {u}"
                        } else { "Waiting for {t.waiting_for}" },
                        secondary: true,
                    )
                    ui.Button("Stop waiting", icon: .play) {
                        on press: resume(t, "")
                    }
                }
            }
            if not t.labels.is_empty() {
                row(gap: .related, wrap: true) {
                    for l in t.labels {
                        ui.Chip(l) {
                            on remove: unlabel(t, l)
                        }
                    }
                }
            }
            if not t.attachments.is_empty() {
                row(gap: .related, wrap: true) {
                    for a in t.attachments {
                        // Opening an attachment is the platform's: the chip hands the file to the
                        // app that opens its kind, and offers taking it off the task beside that.
                        ui.Chip(a.name, icon: .paperclip) {
                            on press: open_attachment(t, a)
                            on remove: detach(t, a)
                        }
                    }
                }
            }
            if let c = t.contact {
                row(gap: .related, align: .center) {
                    ui.Avatar(name: c.name)
                    ui.Text(c.name)
                    ui.Button("Nobody", icon: .user_minus) {
                        on press: assign(t, .none)
                    }
                }
            }
            stack(lead: 1) {
                std.origin(t)
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
                ui.MenuButton(
                    if let e = t.estimate { "{e} points" } else { "Estimate" },
                    icon: .gauge,
                ) {
                    for n in [1, 2, 3, 5, 8, 13] {
                        ui.MenuItem("{n}", checked: t.estimate == .some(n)) {
                            on choose: estimate(t, .some(n))
                        }
                    }
                    ui.MenuItem("No estimate", checked: t.estimate == .none) {
                        on choose: estimate(t, .none)
                    }
                }
                ui.MenuButton(
                    if t.remind == .none { "Remind" } else { "Reminder set" },
                    icon: .bell,
                ) {
                    ui.MenuItem("Remind me…", icon: .bell) {
                        on choose: remind_at(t)
                    }
                    ui.MenuItem("No reminder", icon: .bell_off, checked: t.remind == .none) {
                        on choose: remind_clear(t)
                    }
                }
            }
            if not t.related.is_empty() {
                section("Related") {
                    for r in t.related {
                        ui.ListRow(r.task.title, item: r.task, secondary: r.how, icon: .link)
                    }
                }
            }
            if let w = t.work {
                section("Work") {
                    if w.folder != "" or w.note != "" {
                        row(gap: .related) {
                            if w.folder != "" {
                                ui.Text(w.folder, secondary: true)
                            }
                            if w.note != "" {
                                ui.Text(w.note, secondary: true)
                            }
                        }
                    }
                    let minutes = w.worked_ms / 60000
                    row(gap: .related, wrap: true) {
                        ui.Text("{w.sessions} sessions, {minutes} min", secondary: true)
                        if let budget = w.budget_micros {
                            ui.Text("of {budget} budget", secondary: true)
                        }
                    }
                    for wait in w.waits {
                        ui.Text(
                            if let u = wait.until {
                                "Waiting for {wait.what}, until {u}"
                            } else { "Waiting for {wait.what}" },
                            secondary: true,
                        )
                    }
                    if let s = w.stopped {
                        // Why the work stopped is the line its person reads; the case it was
                        // stopped by stands as the line's glyph.
                        row(gap: .related, align: .center) {
                            ui.Icon(s.why.icon(), size: .small)
                            ui.Text(s.words, secondary: true)
                        }
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
                        (moved, parent, before) -> move_to(moved, parent, before),
                        x -> check(x),
                        progress: x -> subtask_count(x),
                        fill: false,
                    ) { (x, under) ->
                        task_outline_row(x, t.project, under)
                    }
                }
                // A subtask is a task from its first character: the entry makes one with what
                // was typed, so it is a resource at once and another member sees it.
                ui.TextInput(placeholder: "Add a subtask") {
                    on begin(text: Text): add_subtask(t, text) binds .title
                    on blur: remove_empty_step(t)
                }
            }
            std.after_body {
                std.linked(t, without: discussion.said_about(t))
                discussion.thread(t)
            }
        }
    }
}

/// A project's page: what it is, who is involved in it, its tasks and its documents.
view project_page(p: Project) {
    state whom: Text
    std.document(p) {
        stack(gap: .group) {
            stack(gap: .related) {
                // The page's title is the input the window's own bar names it by, alone on its
                // line: what the window puts beside a title stands there.
                ui.TextInput(
                    bind: p.name,
                    placeholder: "What the work goes towards",
                    title: true,
                    fill: 1.0,
                )
                // Whether it is a project or an area is the field's own chip.
                facts(p)
                ui.BlockEditor(bind: p.notes, placeholder: "What it is for.")
            }
            section("People") {
                for person in p.people {
                    row(gap: .related, align: .center) {
                        ui.Avatar(name: person.name)
                        ui.Text(person.name)
                        ui.Text(person.role, secondary: true)
                        ui.Button("Remove", icon: .user_minus) {
                            on press: remove_person(p, person)
                        }
                    }
                }
                ui.TextInput(bind: whom, placeholder: "Add someone") {
                    on submit: involve(p, whom).then(set(whom, ""))
                }
            }
            std.linked(p)
            std.history(p)
        }
    }
}
