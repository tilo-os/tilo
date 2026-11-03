# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

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

d-ftjnco6i = Notes
    .doc = Anything worth remembering about it.

d-2eyau2d6 = Assignee
    .doc = Who is doing it.

d-fbioszhs = Project
    .doc = The project it belongs to; none while it waits in the inbox.

d-v56iknh2 = Section
    .doc = The section of its project it is in, a lane of the project's board; none for the project's first lane.

d-4sriymst = Priority

d-3pf74ifm = Parent
    .doc = The task it is a subtask of; none for a task of its own.

d-2ozuzhiq = Step
    .doc = One step of a task's checklist, from before a task's steps were tasks of their own.
    .noun = { $count ->
        [one] step
       *[other] steps
    }

d-3x2blhsz = Title

d-qbfmff6k = Done

d-idfd5vap = Task
    .doc = The task it is a step of.

d-bfqi332m = Project
    .doc = A project: tasks that work towards one outcome.
    .noun = { $count ->
        [one] project
       *[other] projects
    }

d-sgnau7vd = Name

d-c5pgzhqt = Notes
    .doc = What it is for, and anything worth remembering about it.

d-kfahmlf3 = Board
    .doc = Whether its window shows its tasks as a board, a lane per section, or as a list.

d-5wdrqcpl = Section
    .doc = A section of a project: one lane of its board, such as "Doing".
    .noun = { $count ->
        [one] section
       *[other] sections
    }

d-yfd266wj = Name

d-ohbjjsu7 = Project
    .doc = The project it is a section of.

d-fjqi4yze = Tasks
    .doc = The workspace's tasks and projects.
    .noun = { $count ->
        [one] tasks
       *[other] taskses
    }

d-lg5gedbh = Items
    .doc = Every task, in the order people put them.

d-mdhllpbo = Projects

d-exy7sakn = Steps
    .doc = The steps of every task, before a task's steps were tasks of their own.

d-nk5542m7 = Sections
    .doc = The sections of every project, each naming its project, in the order its board shows them.

d-q6rs4dka = Priority
    .doc = How much a task matters, most urgent last.

d-itg256eu = No priority

d-mq2uqfpl = Low

d-iubwjt4e = Medium

d-clxahlh2 = High

d-d6avlyc5 = Urgent

d-3776c5yx = Row place
    .doc = What the place a task's row stands in already says, and the row therefore leaves out.

d-63qticzp = Nothing: a plain list, a card, a surface of the system's own
    .doc = Nothing: a plain list, a card, a surface of the system's own.

d-u4wzi4k4 = The day it is grouped under, as a list of a day is
    .doc = The day it is grouped under, as a list of a day is.

d-ifftrju4 = Its project, as a project's list is of one
    .doc = Its project, as a project's list is of one.

d-ymvliuk3 = Adopt steps
    .doc = Gives each step of a task from before steps were tasks a task of its own, under the task it was a step of, in the order it was in.

d-u7a3fue4 = Quick add
    .doc = Adds what was typed in a quick add entry as a task, to the inbox or to `p`.

d-rs3ubifz = New task
    .doc = Starts a task in the inbox: one to type into, or one under the title a program or an agent gives it. The entry is how a person says what the task is.

d-zjuy7y7f = New project
    .doc = Starts a project.

d-vwgro4iv = Complete
    .doc = Checks a task off, or opens it again.

d-baqmaej2 = Assign to me
    .doc = Takes a task on: it is the invoker's to do.

d-lwodiwbl = Assign to…
    .doc = Gives a task to someone in the workspace to do.

d-dfu53sij = Snooze until…
    .doc = Puts a task off until a day: it is due then.

d-77umq62t = Unassign
    .doc = Leaves a task for anyone to take.

d-laxsa3x6 = File
    .doc = Files a task under a project.

d-dshqxolf = Move to inbox
    .doc = Takes a task out of its project, back to the inbox.

d-jqhwyztw = Set due date
    .doc = Sets the day a task is due by, or clears it.

d-b5m5wz7r = Prioritize
    .doc = Sets how much a task matters.

d-fykojijd = Add subtask
    .doc = Adds a subtask under a task: a task of its own, in its project and section.

d-berwb3de = Move to
    .doc = Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), where `before` stands when it is given, or at the end of the top level.

d-he6qkhh4 = Indent
    .doc = Moves a task under the task just over it, as Tab does in an outline.

d-khy6tj7s = Outdent
    .doc = Moves a task out from under its parent, just after it, as Shift-Tab does in an outline.

d-ojzyzzxu = Remove
    .doc = Deletes a task and its subtasks.

d-rjpcv6es = Clear done
    .doc = Deletes every finished task, and answers how many went: a task whose subtasks are still undone stays, so clearing never takes unfinished work with it.

d-2xixz4sk = Delete project
    .doc = Deletes a project and its sections; its tasks go back to the inbox.

d-anbqzqxx = Board
    .doc = Shows a project's tasks as a board, a lane per section, or as a list again.

d-2pra5lmb = New section
    .doc = Adds a section at the end of a project's board.

d-xmsyxvog = Rename section
    .doc = Renames a section.

d-vi6srojh = Delete section
    .doc = Deletes a section; its tasks go to the project's first lane.

d-pcn3xgni = New task
    .doc = Adds a task at the end of a lane of a project's board: `s` names the lane, none for the first.

d-scjlkb6d = Place
    .doc = Puts a task into a lane of its project's board, `s` (none for the first lane), at the place `at` among the tasks already there.

d-kjbukd2o = Rename
    .doc = Renames a project.

d-qwizihx4 = Check~present

d-7ldzywu7 = Take~present

d-gqnya75y = Snooze~present

d-7g55zawk = Release~present

d-day53wfn = Unfile~present

d-mg3c777a = Move to~present

d-tpvjqy5m = Clear done~present

d-swaiyhcf = Show board~present

d-i2mhsb3h = Home
    .w-tasks = Tasks
    .w-new-task-for-today = New task for today
    .w-new-task = New task
    .w-no-task-selected = No task selected

d-asnonvwl = Project page
    .w-new-task-in = New task in { $name }
    .w-no-task-selected = No task selected

d-gu3twlv2 = Project board
    .w-no-section = No section
    .w-new-task = New task

d-hc5ndp4x = Task card
    .w-move-to-no-section = Move to No section
    .w-move-to = Move to { $name }

d-w63ba6ad = Task row

d-rj4l44km = Task summary

d-ypbiaiq3 = Task outline row

d-mtmufg3w = Task page
    .w-title = Title
    .w-inbox = Inbox
    .w-subtasks = Subtasks
    .w-add-a-subtask = Add a subtask
    .w-add-notes = Add notes

d-i2mhsb3h-row0 = Home for p

d-i2mhsb3h-row1 = Home for p

d-i2mhsb3h-row2 = Home for x

d-asnonvwl-row0 = Project page for x

d-gu3twlv2-row0 = Project board for s
    .w-new-task = New task

d-gu3twlv2-row1 = Project board for s
    .w-new-task = New task

d-mtmufg3w-row0 = Task page for p

d-mtmufg3w-row1 = Task page for x

d-mtmufg3w-row2-row0 = Task page for c

d-mtmufg3w-row2-row1-row0 = Task page for c

d-mtmufg3w-row2-row1 = Task page for v

d-mtmufg3w-row2 = Task page for s
