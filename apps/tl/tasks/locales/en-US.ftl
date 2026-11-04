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

d-z3mub5ke = Finished
    .doc = Whether it is finished: done itself, and so is every task under it.

d-oddfuemr = Folded
    .doc = Whether it stands in a list's Done tail: finished, and so is every task it is under, so a step done with under a task still open stays where it stands, checked and muted.

d-rc3gzwpo = Subtasks
    .doc = How many tasks are under it, and how many of those are done ("1/3"): the two counts a row and the page's outline show.

d-olnvvb3s = Subtasks done

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

d-vwgro4iv = Complete
    .doc = Checks an item off with every item under it, or opens it again with the items it is under.

d-fykojijd = Add subtask
    .doc = Adds a subtask under an item: an item of its own, at the end of the items it holds, under the title given, keeping what it inherits of the one it stands under.
    .did = added a subtask

d-ymvliuk3 = Adopt steps
    .doc = Gives each step of a task from before steps were tasks a task of its own, under the task it was a step of, in the order it was in.

d-u7a3fue4 = Quick add
    .doc = Adds what was typed in the entry at the end of a list as a task, to the inbox or to `p`, in the lane `s` of its board.

d-rs3ubifz = New task
    .doc = Adds a task under the title a program or an agent gives it: no task stands without one.

d-zjuy7y7f = New project
    .doc = Starts a project.

d-tmwdrpev = Today
    .doc = Sets the day a task is due by, or clears it: the presets take the day, and one entry asks for any day.

d-wclcgiyt = Tomorrow
    .doc = Makes a task due tomorrow.

d-tg5yjcju = Next week
    .doc = Makes a task due at the end of next week.

d-jqhwyztw = Pick date
    .doc = Makes a task due on a day the person picks.

d-x5xnqddo = No date
    .doc = Clears the day a task is due by: it waits in the inbox, undated.

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

d-rjpcv6es = Delete finished tasks
    .doc = Deletes every finished task, and answers how many went: a task is finished when everything under it is done too, so deleting takes whole finished branches and never unfinished work with it, and a task done with a step still open under it stays.

d-2xixz4sk = Delete project
    .doc = Deletes a project and its sections; its tasks go back to the inbox.

d-2pra5lmb = New section
    .doc = Adds a section at the end of a project's board.

d-xmsyxvog = Rename section
    .doc = Renames a section.

d-vi6srojh = Delete section
    .doc = Deletes a section; its tasks go to the project's first lane.

d-pcn3xgni = New task
    .doc = Adds a task to a project, at the end of the lane `s` of its board (none for the first), under a title a program or an agent gives it.

d-scjlkb6d = Place
    .doc = Puts a task into a lane of its project's board, `s` (none for the first lane), at the place `at` among the tasks already there.

d-kjbukd2o = Rename
    .doc = Renames a project.

d-berwb3de = Move to
    .doc = Moves an item to a place in the outline: at the end of `parent`'s items (the top level for none), where `before` stands when it is given, or at the end of the top level.

d-he6qkhh4 = Indent
    .doc = Moves an item under the item the view shows just over it in its group, as `Ctrl+]` does: the row over it names the item of its own level it ends up under.

d-khy6tj7s = Outdent
    .doc = Moves an item out from under its parent, just after it, as `Ctrl+[` does in an outline.

d-23y3zdgu = Duplicate
    .doc = Copies an item, with every item under it, just after the one it copies.

d-ojzyzzxu = Delete
    .doc = Deletes an item and the items under it.

d-m2xarbfd = Copy below
    .doc = Copies the items under `t` under `copy`, each in its place, so a copy reads as the item it copies did; answers the last item the copies end with.

d-qwizihx4 = Check~present

d-mdzqe3gc = Due today~present

d-rnagsir2 = Due tomorrow~present

d-fym3mxqc = Due next week~present

d-ohw2ks6t = Due clear~present

d-dpworviv = Set priority none~present

d-vh3rraia = Set priority low~present

d-ir7grgoz = Set priority medium~present

d-g4wmwcnr = Set priority high~present

d-lvbdxuzj = Set priority urgent~present

d-day53wfn = Unfile~present

d-tpvjqy5m = Clear done~present

d-i2mhsb3h = Home
    .w-tasks = Tasks
    .w-no-task-selected = No task selected

d-7froalwb = Project list
    .w-no-task-selected = No task selected

d-gu3twlv2 = Project board

d-hc5ndp4x = Task card
    .w-remove-from-section = Remove from section
    .w-move-to = Move to { $name }

d-w63ba6ad = Task row

d-rj4l44km = Task summary

d-ypbiaiq3 = Task outline row

d-mtmufg3w = Task page
    .w-subtasks = Subtasks
    .w-add-a-subtask = Add a subtask
    .w-inbox = Inbox

d-mg3c777a = Move to~present
