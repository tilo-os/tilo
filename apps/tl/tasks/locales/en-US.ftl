# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-tasks = Tasks

d-so-tilo-tasks-keywords = tasks, issues, issue tracker, kanban, board, to-do, todo, tickets, projects, agent tasks

d-snj4onfb = Person
    .doc = Someone involved in a project, with the role they have there: who a task can be given to without typing them in again.
    .noun = { $count ->
        [one] person
       *[other] people
    }

d-ktw2jimm = Name

d-hgptkohm = Role

d-bfqi332m = Project
    .doc = A project or an area: the outcome tasks work towards, and who is involved in it.
    .noun = { $count ->
        [one] project
       *[other] projects
    }

d-sgnau7vd = Name

d-rsoasw3n = Kind

d-c5pgzhqt = Notes
    .doc = What it is for, and anything worth remembering about it.

d-mboq6kfe = People

d-bfqi332m-filter-kind = Kind

d-7jh5u6dc = Wait
    .doc = What a task waits for, in a few words.
    .noun = { $count ->
        [one] wait
       *[other] waits
    }

d-4fz6j3rg = Kind

d-4ijm32x7 = What
    .doc = What it waits for: a reply, a moment, a resource, the pieces.

d-dy6nui5s = Until
    .doc = The latest moment the wait ends, whether or not what it waits for came.

d-osxm3ega = Event
    .doc = The automation that ends the wait early when it runs, by its id.

d-7jh5u6dc-filter-kind = Kind

d-7jh5u6dc-filter-until = Until

d-p634b5uh = Stopped
    .doc = That a task's work stopped, and why.
    .noun = { $count ->
        [one] stopped
       *[other] stoppeds
    }

d-ps5lbw2w = Why

d-5yqkyqzr = Words
    .doc = The plain line its person reads.

d-kdugvhf2 = At

d-p634b5uh-filter-why = Why

d-p634b5uh-filter-at = At

d-3bdn2bkx = Resumed
    .doc = What a task's work waited for, and what made it go on.
    .noun = { $count ->
        [one] resumed
       *[other] resumeds
    }

d-66a5tqcs = Cause

d-ys7neba6 = At

d-3bdn2bkx-filter-at = At

d-pwfklw2l = Work
    .doc = What a task keeps while an agent works it over many sessions: where its working material is, what it waits for, what it may cost, and how long it worked.
    .noun = { $count ->
        [one] work
       *[other] works
    }

d-finhtl5k = Folder
    .doc = The folder of the workspace where its material is kept, by its address.

d-ckijp22f = Note
    .doc = The note its sessions write, by its address.

d-3hituttb = Waits
    .doc = What it waits for: none while it waits for nothing.

d-fxyk36kk = Until
    .doc = The latest moment its latest wait ends.

d-pcmbdaod = Since
    .doc = When the wait began.

d-lkkui5te = Budget micros
    .doc = What it may spend, in a millionth of the workspace's money; none for no limit.

d-o5tuxt2b = Budget ms
    .doc = What it may work, in milliseconds; none for no limit.

d-byxiukp2 = Budget by
    .doc = Whose budget the limits charge.

d-4c4aw4xd = Spent micros
    .doc = What its sessions spent, in a millionth of the workspace's money.

d-virxpj23 = Worked ms
    .doc = How long its sessions worked, in milliseconds.

d-hztvc5dj = Sessions
    .doc = How many sessions worked it.

d-nuikd6fz = Session since
    .doc = When the session now working it began.

d-qublsm3d = Lost
    .doc = How many sessions ended without finishing.

d-nax7z7kw = Stopped
    .doc = Why its work stopped, with the words it did.

d-55waklcc = Resumed
    .doc = What made it go on after a wait.

d-huwemkpv = Pieces at once
    .doc = How many of its pieces may be in work at once.

d-pwfklw2l-filter-until = Until

d-pwfklw2l-filter-since = Since

d-pwfklw2l-filter-budget_micros = Budget micros

d-pwfklw2l-filter-budget_ms = Budget ms

d-pwfklw2l-filter-budget_by = Budget by

d-pwfklw2l-filter-spent_micros = Spent micros

d-pwfklw2l-filter-worked_ms = Worked ms

d-pwfklw2l-filter-sessions = Sessions

d-pwfklw2l-filter-session_since = Session since

d-pwfklw2l-filter-lost = Lost

d-pwfklw2l-filter-stopped = Stopped

d-pwfklw2l-filter-resumed = Resumed

d-pwfklw2l-filter-pieces_at_once = Pieces at once

d-2nkvzgb3 = Origin
    .doc = Where it was first written, when it was moved here from another tracker or service: the person there, the service, where it reads there and its reference, so a task or a thread imported from elsewhere says where it came from.
    .noun = { $count ->
        [one] origin
       *[other] origins
    }

d-3saq3nij = Author
    .doc = Who wrote it there, as they were named.

d-duzjakqx = Handle
    .doc = The name they were known by there, when it differs.

d-g6jrgwt6 = Source
    .doc = The service it came from.

d-gncjc3dz = Url
    .doc = Where it reads there.

d-qg5qvqsi = Reference
    .doc = Its reference there, such as its number.

d-xuslp5pg = Created
    .doc = When it was first written there.

d-no47vg4q = Updated
    .doc = When it last changed there.

d-2nkvzgb3-filter-created = Created

d-2nkvzgb3-filter-updated = Updated

d-75ojxgs3 = Task
    .doc = Something to do: what, by when, by whom, in which project and where in its work.
    .noun = { $count ->
        [one] task
       *[other] tasks
    }

d-2y3yxgz4 = Number
    .doc = Its number in the workspace (`#12`), given once when it is made.

d-oidkt4g5 = Title

d-ftjnco6i = Notes

d-2xssorsk = Status

d-fgvmlm5n = Done
    .doc = Whether it is finished, which its status says: a row's box edits a flag, so the flag the status means is kept beside it and every action that writes the status writes it too.

d-g6kykjp2 = Due
    .doc = The day it is due by, if any.

d-qadph346 = At
    .doc = The moment on that day it is due at, if it has one.

d-aeilucpg = Remind
    .doc = When to tell whoever is doing it that its moment came, if a reminder is set.

d-2eyau2d6 = Assignee
    .doc = Who is doing it.

d-ptyivutv = Assigned by
    .doc = Who gave it to the agent it is assigned to.

d-r5bd6ss3 = Assigned at
    .doc = When it was given to that agent.

d-tj32pwsz = Labels
    .doc = The words people sort it by across projects, such as "bug" or "errand".

d-fbioszhs = Project
    .doc = The project it belongs to; none while it waits in the inbox.

d-4sriymst = Priority

d-nc5peol7 = Estimate
    .doc = How much work it is, in points.

d-3pf74ifm = Parent
    .doc = The task it is a subtask of, to any depth; none for a task of its own.

d-zjuuqsza = Attachments
    .doc = Files attached to it.

d-sqy7cbtd = Origin
    .doc = Where it was first written, when it was moved here from another tracker.

d-o6ijslz7 = Url
    .doc = The page it is read at elsewhere.

d-3uvt7tnr = Work
    .doc = What it keeps while an agent works it.

d-uxlestrl = Waiting for
    .doc = What it waits for, in a few words: empty while it waits for nothing.

d-m6raucmj = Waiting until
    .doc = The latest moment its wait ends, so its wait ends when it comes.

d-vtgi2fsa = Waiting on
    .doc = The automation that ends its wait early, when one does.

d-75ojxgs3-filter-all_issues = All issues

d-75ojxgs3-filter-inbox = Inbox

d-75ojxgs3-filter-today = Today

d-75ojxgs3-filter-upcoming = Upcoming

d-75ojxgs3-filter-todos = Todos

d-75ojxgs3-filter-done = Done

d-75ojxgs3-filter-assigned_to_me = Assigned to me

d-75ojxgs3-filter-nobody_yet = Nobody yet

d-75ojxgs3-filter-agent_work = Agent work

d-75ojxgs3-filter-waiting = Waiting

d-75ojxgs3-filter-due = Due

d-75ojxgs3-filter-project = Project

d-75ojxgs3-filter-priority = Priority

d-75ojxgs3-filter-parent = Parent

d-75ojxgs3-filter-labels = Labels

d-75ojxgs3-filter-status = Status

d-75ojxgs3-filter-assignee = Assignee

d-fjqi4yze = Tasks
    .doc = The workspace's tasks and projects.
    .noun = { $count ->
        [one] task
       *[other] tasks
    }

d-lg5gedbh = Items
    .doc = Every task, in the order people put them.

d-mdhllpbo = Projects

d-c7peh6l3 = Origins
    .doc = Where the tasks and threads here were first written: one for each place they came from.

d-auinwnh3 = Numbers
    .doc = The highest number given so far: the next task's is one more.

d-fjqi4yze-filter-numbers = Numbers

d-q6rs4dka = Priority
    .doc = How much a task matters, most urgent last.

d-itg256eu = No priority

d-mq2uqfpl = Low

d-iubwjt4e = Medium

d-clxahlh2 = High

d-d6avlyc5 = Urgent

d-lst5n5qw = Status
    .doc = Where a task stands in its work: a board's columns, fixed so that every task of the workspace reads in the same place in the same column.

d-gifhllm2 = Written down, not planned yet
    .doc = Written down, not planned yet.

d-ecladcck = Planned: the next thing to pick up
    .doc = Planned: the next thing to pick up.

d-743ubdnw = Someone is working on it
    .doc = Someone is working on it.

d-uoo2c3nt = Done by whoever worked on it, waiting for a person to look
    .doc = Done by whoever worked on it, waiting for a person to look.

d-s2aii2do = Done and checked by its person
    .doc = Done and checked by its person.

d-m3y6yj4c = Not to be done
    .doc = Not to be done.

d-bknmpaiw = Kind
    .doc = Which kind of project this is.

d-y4ia6lqx = A finite goal: the work ends, and so does the project
    .doc = A finite goal: the work ends, and so does the project.

d-erfezq3t = An ongoing responsibility that has no end, only what is done next
    .doc = An ongoing responsibility that has no end, only what is done next.

d-i4njiseg = Budget by
    .doc = Whose budget a task's work spends.

d-k2qa6dfv = Its person's
    .doc = Its person's.

d-x5hbaxqb = The agent working on it, which holds itself to it
    .doc = The agent working on it, which holds itself to it.

d-awbl26xb = Wait kind
    .doc = What a wait waits for: the kinds a session of an agent tells apart.

d-gmuoiqeq = A moment: nothing else ends it
    .doc = A moment: nothing else ends it.

d-ce3ohtpj = A reply: in a mail thread, a conversation
    .doc = A reply: in a mail thread, a conversation.

d-wfyj2bwt = A resource of the workspace changing
    .doc = A resource of the workspace changing.

d-bv56uipx = An answer of the person the work is for: the task needs them
    .doc = An answer of the person the work is for: the task needs them.

d-37r6d2ov = Answers of several people
    .doc = Answers of several people.

d-xmhsnlsk = Its pieces, the tasks under it: it goes on when none of them is still to be worked on
    .doc = Its pieces, the tasks under it: it goes on when none of them is still to be worked on.

d-vcxpkpb6 = Stopped why
    .doc = Why a task's work stopped.

d-zpiqqdxk = It could not go on: the line says what failed
    .doc = It could not go on: the line says what failed.

d-rvil2upx = Its budget in money is spent
    .doc = Its budget in money is spent.

d-3xiu65sl = Its budget in working time is spent
    .doc = Its budget in working time is spent.

d-jipsu7zw = Its agent sessions were interrupted and did not come back
    .doc = Its agent sessions were interrupted and did not come back.

d-odjzaby6 = A person stopped it
    .doc = A person stopped it.

d-rs3ubifz = New task
    .doc = Adds a task to the inbox or to `p` at `status`: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so "Call the landlord fri 10am @Bo" is a task due Friday at ten, for Bo.

d-rs3ubifz-p = P

d-rs3ubifz-status = Status

d-rs3ubifz-due = Due

d-rs3ubifz-at = At

d-rs3ubifz-assignee = Assignee

d-rs3ubifz-title = Title

d-rs3ubifz-remind = Remind

d-2rpw5inw = New tasks
    .doc = Adds every task of `titles` at once, as one change: none of them is made when one cannot be (a blank title), so a person's batch is whole or nothing.
    .did = added tasks
    .w-a-task-needs-a-title = A task needs a title

d-2rpw5inw-titles = Titles

d-2rpw5inw-p = P

d-zjuy7y7f = New project
    .doc = Starts a project.
    .w-new-project = New project

d-cando5zh = Kind
    .doc = Sets whether a project is one with an end, or an ongoing responsibility.
    .w-it-has-that-kind-already = It has that kind already

d-cando5zh-kind = Kind

d-kjbukd2o = Rename
    .doc = Renames a project.

d-kjbukd2o-name = Name

d-5bh4papz = Add person
    .doc = Adds someone involved in a project, with the role they have there.
    .w-name-the-person = Name the person

d-5bh4papz-name = Name

d-5bh4papz-role = Role

d-yilfqr2i = Remove person
    .doc = Takes someone off a project.

d-yilfqr2i-person = Person

d-2xixz4sk = Delete project
    .doc = Deletes a project; its tasks go back to the inbox.

d-pcn3xgni = New task
    .doc = Adds a task to a project at `status`.

d-pcn3xgni-title = Title

d-pcn3xgni-status = Status

d-bz2p62js = Rename
    .doc = Sets what a task is called.
    .w-a-task-needs-a-title = A task needs a title

d-bz2p62js-title = Title

d-berwb3de = Move to
    .doc = Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands.
    .w-a-task-cannot-go-inside-itself = A task cannot go inside itself

d-berwb3de-parent = Parent

d-berwb3de-before = Before

d-he6qkhh4 = Indent
    .doc = Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does; with none named, under the task above it in the list.
    .w-nothing-over-it-to-move-under = Nothing over it to move under

d-he6qkhh4-above = Above

d-khy6tj7s = Outdent
    .doc = Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline.
    .w-it-is-at-the-top-level = It is at the top level already

d-o3lvuj36 = Status
    .doc = Puts a task in a column of the board, at the place `at` among the tasks already there: what a card's drag names.
    .did = moved
    .w-it-stands-there-already = It stands there already

d-o3lvuj36-status = Status

d-o3lvuj36-at = At

d-zzrudiur = Move to
    .doc = Puts every chosen task in one column (or project) at once, as one change.
    .did = moved tasks
    .w-choose-the-tasks-to-move = Choose the tasks to move

d-zzrudiur-chosen = Chosen

d-zzrudiur-status = Status

d-zzrudiur-at = At

d-vwgro4iv = Reopen
    .doc = Checks a task off with every task under it, or opens it again with the tasks it is under.
    .w-complete = Complete
    .w-reopen = Reopen

d-vwgro4iv-done = Done

d-fykojijd = Add subtask
    .doc = Adds a subtask under a task, in its project and column, and opens a closed one: there is work under it now.
    .did = added a subtask

d-fykojijd-title = Title

d-w2qapnpp = Remove empty step
    .doc = Takes away the subtask the entry left empty: a field that began one and was left blank is no step of the work, so nothing empty stands under the task.
    .did = removed an empty step

d-tmwdrpev = Today
    .doc = Sets the day a task is due by, from the presets or a day the person picks.
    .w-it-is-done-already = It is done already

d-wclcgiyt = Tomorrow
    .doc = Makes a task due tomorrow.
    .w-it-is-done-already = It is done already

d-tg5yjcju = Next week
    .doc = Makes a task due at the start of next week.
    .w-it-is-done-already = It is done already

d-jqhwyztw = Pick date
    .doc = Makes a task due on a day the person picks.

d-jqhwyztw-due = Due

d-dewj7mzg = Set time
    .doc = Sets the time of day a task is due at: the day it is on stays, and a task with no day is due on the day the moment names.

d-dewj7mzg-at = At

d-x5xnqddo = No date
    .doc = Clears the day a task is due by: it waits in the inbox, undated.
    .w-it-has-no-date = It has no date

d-3qq23ybf = Remind me
    .doc = Sets the moment whoever is doing a task is reminded of it.

d-3qq23ybf-when = When

d-vjsjvx7u = No reminder
    .doc = Clears a task's reminder: it is not told of again.
    .w-it-has-no-reminder = It has no reminder

d-7vjyuy7c = Remind
    .doc = Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already closed.
    .w-due = Due { $d }

d-w43ij7uu = No priority
    .doc = Sets how much a task matters.
    .w-it-has-no-priority = It has no priority

d-pdc7ukxu = Low
    .doc = Marks a task as low priority.
    .w-it-is-low-already = It is low already

d-cpcxkauo = Medium
    .doc = Marks a task as medium priority.
    .w-it-is-medium-already = It is medium already

d-kglmmfa7 = High
    .doc = Marks a task as high priority.
    .w-it-is-high-already = It is high already

d-lrdm4l3c = Urgent
    .doc = Marks a task as urgent.
    .w-it-is-urgent-already = It is urgent already

d-xgmuq3oy = Estimate
    .doc = Sets the points a task is worth, or none.

d-xgmuq3oy-points = Points

d-laxsa3x6 = Project
    .doc = Files a task under a project the person picks.

d-laxsa3x6-p = P

d-dshqxolf = Inbox
    .doc = Takes a task out of its project, back to the inbox.
    .w-it-is-in-the-inbox-already = It is in the inbox already

d-c6y4cxdd = Assign
    .doc = Gives a task to someone, or to nobody.
    .w-the-pieces-in-work-already-fill = The pieces in work already fill what this task lets work at once
    .w-it-is-theirs-already = It is theirs already

d-c6y4cxdd-who = Who

d-jwva2bcv = Assign
    .doc = Gives every chosen task to one person (or nobody), as one change.
    .did = assigned tasks
    .w-choose-the-tasks-to-assign = Choose the tasks to assign

d-jwva2bcv-chosen = Chosen

d-jwva2bcv-who = Who

d-fs7nzreh = Label…
    .doc = Gives a task a label.
    .w-name-the-label = Name the label

d-fs7nzreh-name = Name

d-eocwppga = Remove label
    .doc = Takes a label off a task.
    .w-it-has-no-such-label = It has no such label

d-eocwppga-name = Name

d-pk6rzcqw = Label…
    .doc = Puts every chosen task under one label, as one change.
    .did = labelled tasks
    .w-name-the-label = Name the label

d-pk6rzcqw-chosen = Chosen

d-pk6rzcqw-name = Name

d-3h5dpt4p = Attach file
    .doc = Attaches a file to a task.
    .did = attached a file

d-3h5dpt4p-file = File

d-uqpc3g6w = Remove attachment
    .doc = Takes an attachment off a task.

d-uqpc3g6w-attachment = Attachment

d-xrr33hkc = Import files
    .doc = Imports files as tasks: one task for each file, under the project `p` when one is named, the file attached to it.
    .did = imported files
    .w-choose-the-files-to-import = Choose the files to import

d-xrr33hkc-files = Files

d-xrr33hkc-p = P

d-6tlw3ran = Add origin
    .doc = Starts a task's work on another tracker's issue: its origin, its page there and its number, and a task of the same origin already here is that task, so importing it again makes none.
    .did = named where it came from

d-6tlw3ran-author = Author

d-6tlw3ran-source = Source

d-6tlw3ran-handle = Handle

d-6tlw3ran-url = Url

d-6tlw3ran-reference = Reference

d-6tlw3ran-created = Created

d-6tlw3ran-updated = Updated

d-jfoiwgwq = Import issue
    .did = imported an issue
    .w-an-imported-issue-needs-a-key = An imported issue needs a key

d-jfoiwgwq-title = Title

d-jfoiwgwq-key = Key

d-jfoiwgwq-status = Status

d-p4wi6ohi = Write its note
    .doc = Writes a task's note in Notes: a note named like the task, holding what it remembers, made from the task so it reads under it.
    .did = wrote a note

d-3ncicl2v = Wait for…
    .doc = Puts a task on hold until what it waits for comes, or until `until` at the latest: whoever is doing it goes on with it then, a person or an agent alike.
    .did = put on hold
    .w-it-is-done-already = It is done already
    .w-say-what-it-waits-for = Say what it waits for
    .w-the-wait-ends-in-the-past = The wait ends in the past

d-3ncicl2v-what = What

d-3ncicl2v-until = Until

d-3ncicl2v-kind = Kind

d-3ncicl2v-on = On

d-ab4v7pl5 = Stop waiting
    .doc = Ends every wait a task is on: what it waited for came, which `cause` says in a few words, and whoever is doing it goes on with it.
    .did = went on with
    .w-it-waits-for-nothing = It waits for nothing

d-ab4v7pl5-cause = Cause

d-tji6xijt = Working place
    .doc = Sets where a task's working material is: the folder of its work, and the note its sessions write.
    .did = set the working place

d-tji6xijt-folder = Folder

d-tji6xijt-note = Note

d-l7p7vczl = Budget
    .doc = Sets what a task's work may spend and how long it may work, and whose budget it charges.
    .did = set a budget

d-l7p7vczl-micros = Micros

d-l7p7vczl-millis = Millis

d-l7p7vczl-by = By

d-bx72tblu = Pieces at once
    .doc = How many of a task's pieces may be in work at once.
    .did = set the pieces at once

d-bx72tblu-pieces = Pieces

d-6k5mm32z = Session began
    .doc = Says a session on a task began: counted, and its start kept for how long it works.
    .did = began work
    .w-i-stopped-the-working-time-this = I stopped: the working time this task was given is used up. Give it more time to let me go on.
    .w-i-stopped-this-task-s-budget = I stopped: this task's budget is spent. Raise it to let me go on.
    .w-i-stopped-my-work-on-this = I stopped: my work on this was interrupted too often in a row. Tell me to go on and I start again from its notes.

d-lvgvolrt = Session ended
    .doc = Says a session on a task ended: what it spent and how long it worked are added to the task.
    .did = ended work

d-lvgvolrt-spent_micros = Spent micros

d-lvgvolrt-worked_ms = Worked ms

d-y5jf4p7n = Stop
    .doc = Says a task's work stopped, and why, in the words its person reads.
    .did = stopped work

d-y5jf4p7n-why = Why

d-y5jf4p7n-words = Words

d-23y3zdgu = Duplicate
    .doc = Copies a task, with every task under it, just after the one it copies.

d-ojzyzzxu = Delete
    .doc = Deletes a task and its subtasks.

d-rjpcv6es = Delete finished tasks
    .doc = Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays.
    .w-nothing-is-finished-yet = Nothing is finished yet

d-5ohbulhj = Share with
    .doc = Gives the copy `to` what `from` holds in its set, its list and its collection: what only an action may change.
    .did = copied a task

d-5ohbulhj-from = From

d-cfituto7 = New tasks~present

d-r3lxwimd = Set kind~present

d-da66j22u = Involve~present

d-ym2iynlv = Retitle~present

d-mg3c777a = Move to~present

d-ajdiznj3 = Set status~present

d-2vvqvs5e = Set status many~present

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

d-r7b5neqa = Assign~present

d-x3jqfzjh = Assign many~present

d-vtz6f5ah = Label~present

d-szsksrsg = Unlabel~present

d-m667p5oi = Label many~present

d-2b46hb6u = Import files~present

d-tk5t43h6 = Import issue~present

d-fnc6zwh7 = Wait~present

d-pgzbwzsm = Session began~present

d-tpvjqy5m = Clear done~present

d-i2mhsb3h = Home
    .w-all-issues = All issues
    .w-new-task-labelled = New task labelled { $label }
    .w-new-task-in = New task in { $name }
    .w-new-task-for-today = New task for today
    .w-new-task = New task
    .w-tasks = Tasks
    .w-projects = Projects
    .w-labels = Labels
    .w-no-tasks-here = No tasks here

d-mikudv5u = Board
    .w-tasks = Tasks
    .w-new-task = New task

d-q4eq7oqv = To do
    .w-new-task = New task
    .w-nothing-to-do = Nothing to do

d-doqleehr = Task item
    .w-what-to-do = What to do
    .w-from = From { $source } { $r }
    .w-from-2 = From { $source }
    .w-waiting-for-until = Waiting for { $waiting_for }, until { $u }
    .w-waiting-for = Waiting for { $waiting_for }
    .w-stop-waiting = Stop waiting
    .w-anything-worth-remembering-about-it = Anything worth remembering about it.
    .w-inbox = Inbox
    .w-points = { $e } points
    .w-estimate = Estimate
    .w-no-estimate = No estimate
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-remind-me = Remind me…
    .w-no-reminder = No reminder
    .w-work = Work
    .w-sessions-min = { $sessions } sessions, { $minutes } min
    .w-of-budget = of { $budget } budget
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
    .w-move-to = Move to { $label }

d-w63ba6ad = Task row

d-rj4l44km = Task summary

d-ypbiaiq3 = Task outline row

d-mtmufg3w = Task page
    .w-what-to-do = What to do
    .w-from = From { $source } { $r }
    .w-from-2 = From { $source }
    .w-waiting-for-until = Waiting for { $waiting_for }, until { $u }
    .w-waiting-for = Waiting for { $waiting_for }
    .w-stop-waiting = Stop waiting
    .w-anything-worth-remembering-about-it = Anything worth remembering about it.
    .w-inbox = Inbox
    .w-points = { $e } points
    .w-estimate = Estimate
    .w-no-estimate = No estimate
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-remind-me = Remind me…
    .w-no-reminder = No reminder
    .w-work = Work
    .w-sessions-min = { $sessions } sessions, { $minutes } min
    .w-of-budget = of { $budget } budget
    .w-subtasks = Subtasks
    .w-no-subtasks-yet = No subtasks yet
    .w-add-a-subtask = Add a subtask

d-fxb6wc2q = Project page
    .w-what-the-work-goes-towards = What the work goes towards
    .w-what-it-is-for = What it is for.
    .w-people = People
    .w-add-someone = Add someone
