# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-tasks = Tasks

d-75ojxgs3 = Task
    .doc = Something to do: what, by when, by whom, and in which project.
    .noun = { $count ->
        [one] task
       *[other] tasks
    }

d-oidkt4g5 = Title

d-fgvmlm5n = Done

d-g6kykjp2 = Due
    .doc = The day it is due by, if any.

d-qadph346 = At
    .doc = The moment on that day it is due at, if it has one.

d-aeilucpg = Remind
    .doc = When to tell whoever is doing it that its moment came, if a reminder is set.

d-ftjnco6i = Notes
    .doc = Anything worth remembering about it.

d-2eyau2d6 = Assignee
    .doc = Who is doing it.

d-fbioszhs = Project
    .doc = The project it belongs to; none while it waits in the inbox.

d-v56iknh2 = Section
    .doc = The lane of its project's board it is in; none for the first lane.

d-4sriymst = Priority

d-3pf74ifm = Parent
    .doc = The task it is a subtask of, to any depth; none for a task of its own.

d-75ojxgs3-filter-inbox = Inbox

d-75ojxgs3-filter-today = Today

d-75ojxgs3-filter-upcoming = Upcoming

d-75ojxgs3-filter-done = Done

d-75ojxgs3-filter-assigned_to_me = Assigned to me

d-75ojxgs3-filter-due = Due

d-75ojxgs3-filter-project = Project

d-75ojxgs3-filter-priority = Priority

d-75ojxgs3-filter-parent = Parent

d-75ojxgs3-filter-assignee = Assignee

d-bfqi332m = Project
    .doc = A project: tasks that work towards one outcome.
    .noun = { $count ->
        [one] project
       *[other] projects
    }

d-sgnau7vd = Name

d-c5pgzhqt = Notes
    .doc = What it is for, and anything worth remembering about it.

d-5wdrqcpl = Section
    .doc = A section of a project: one lane of its board, such as "Doing".
    .noun = { $count ->
        [one] section
       *[other] sections
    }

d-yfd266wj = Name

d-ohbjjsu7 = Project
    .doc = The project it is a section of.

d-5wdrqcpl-filter-project = Project

d-fjqi4yze = Tasks
    .doc = The workspace's tasks and projects.
    .noun = { $count ->
        [one] tasks
       *[other] taskses
    }

d-lg5gedbh = Items
    .doc = Every task, in the order people put them.

d-mdhllpbo = Projects

d-nk5542m7 = Sections
    .doc = The sections of every project, in the order its board shows them.

d-q6rs4dka = Priority
    .doc = How much a task matters, most urgent last.

d-itg256eu = No priority

d-mq2uqfpl = Low

d-iubwjt4e = Medium

d-clxahlh2 = High

d-d6avlyc5 = Urgent

d-3776c5yx = Row place
    .doc = What the place a task's row stands in already says, and the row therefore leaves out.

d-u4wzi4k4 = The day it is grouped under, as a list of a day is
    .doc = The day it is grouped under, as a list of a day is.

d-ifftrju4 = Its project, as a project's list is of one
    .doc = Its project, as a project's list is of one.

d-rs3ubifz = New task
    .doc = Adds a task to the inbox or to `p`, in the lane `s` of its board: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so "Call the landlord fri 10am @Bo" is a task due Friday at ten, for Bo.

d-zjuy7y7f = New project
    .doc = Starts a project.

d-vwgro4iv = Complete
    .doc = Checks a task off with every task under it, or opens it again with the tasks it is under.

d-fykojijd = Add subtask
    .doc = Adds a subtask under a task, in its project and section, and opens a done one: there is work under it now.
    .did = added a subtask

d-tmwdrpev = Today
    .doc = Sets the day a task is due by, from the presets or a day the person picks.

d-wclcgiyt = Tomorrow
    .doc = Makes a task due tomorrow.

d-tg5yjcju = Next week
    .doc = Makes a task due at the start of next week.

d-jqhwyztw = Pick date
    .doc = Makes a task due on a day the person picks.

d-x5xnqddo = No date
    .doc = Clears the day a task is due by: it waits in the inbox, undated.

d-3qq23ybf = Remind me
    .doc = Sets the moment whoever is doing a task is reminded of it.

d-vjsjvx7u = No reminder
    .doc = Clears a task's reminder: it is not told of again.

d-7vjyuy7c = Remind
    .doc = Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already done.
    .w-due = Due { $d }

d-w43ij7uu = No priority
    .doc = Sets how much a task matters.

d-pdc7ukxu = Low
    .doc = Marks a task as low priority.

d-cpcxkauo = Medium
    .doc = Marks a task as medium priority.

d-kglmmfa7 = High
    .doc = Marks a task as high priority.

d-lrdm4l3c = Urgent
    .doc = Marks a task as urgent.

d-laxsa3x6 = Project
    .doc = Files a task under a project the person picks.

d-dshqxolf = Inbox
    .doc = Takes a task out of its project, back to the inbox.

d-c6y4cxdd = Assign
    .doc = Gives a task to someone, or to nobody.

d-berwb3de = Move to
    .doc = Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands.

d-he6qkhh4 = Indent
    .doc = Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does.

d-khy6tj7s = Outdent
    .doc = Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline.

d-23y3zdgu = Duplicate
    .doc = Copies a task, with every task under it, just after the one it copies.

d-ojzyzzxu = Delete
    .doc = Deletes a task and its subtasks.

d-rjpcv6es = Delete finished tasks
    .doc = Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays.

d-2xixz4sk = Delete project
    .doc = Deletes a project and its sections; its tasks go back to the inbox.

d-2pra5lmb = New section
    .doc = Adds a section at the end of a project's board.

d-xmsyxvog = Rename section
    .doc = Renames a section.

d-vi6srojh = Delete section
    .doc = Deletes a section; its tasks go to the project's first lane.

d-pcn3xgni = New task
    .doc = Adds a task to a project, at the end of the lane `s` of its board (none for the first).

d-scjlkb6d = Place in board
    .doc = Puts a task into the lane `s` of its project's board (none for the first) at the place `at` among the tasks already there: what a card's drag names.

d-kjbukd2o = Rename
    .doc = Renames a project.

d-qwizihx4 = Check~present

d-mdzqe3gc = Due today~present

d-rnagsir2 = Due tomorrow~present

d-fym3mxqc = Due next week~present

d-ohw2ks6t = Due clear~present

d-g7kx77ng = Remind clear~present

d-dpworviv = Set priority none~present

d-vh3rraia = Set priority low~present

d-ir7grgoz = Set priority medium~present

d-g4wmwcnr = Set priority high~present

d-lvbdxuzj = Set priority urgent~present

d-day53wfn = Unfile~present

d-mg3c777a = Move to~present

d-tpvjqy5m = Clear done~present

d-rxpe5yqk = Place~present

d-i2mhsb3h = Home
    .w-tasks = Tasks
    .w-projects = Projects
    .w-no-tasks-here = No tasks here

d-doqleehr = Task item
    .w-what-to-do = What to do
    .w-anything-worth-remembering-about-it = Anything worth remembering about it.
    .w-inbox = Inbox
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-remind-me = Remind me…
    .w-no-reminder = No reminder
    .w-subtasks = Subtasks
    .w-no-subtasks-yet = No subtasks yet
    .w-add-a-subtask = Add a subtask
    .w-open-as-page = Open as page

d-7froalwb = Project list
    .w-new-task-in = New task in { $name }
    .w-no-tasks-yet = No tasks yet

d-gu3twlv2 = Project board
    .w-new-task = New task

d-hc5ndp4x = Task card
    .w-remove-from-section = Remove from section
    .w-move-to = Move to { $name }

d-w63ba6ad = Task row

d-rj4l44km = Task summary

d-ypbiaiq3 = Task outline row

d-mtmufg3w = Task page
    .w-what-to-do = What to do
    .w-anything-worth-remembering-about-it = Anything worth remembering about it.
    .w-inbox = Inbox
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-remind-me = Remind me…
    .w-no-reminder = No reminder
    .w-subtasks = Subtasks
    .w-no-subtasks-yet = No subtasks yet
    .w-add-a-subtask = Add a subtask
