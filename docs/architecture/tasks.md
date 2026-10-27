# Tasks

Work in a workspace is tracked as issues in the Tasks app (`apps/tasks`,
`so.tilo.tasks`). An issue is a resource like any other: it has a number, a
title, a markdown body, a status, an assignee, labels, a parent, links to other
resources and a thread. An agent task is an issue assigned to an agent.

## The resource

The tracker is the Tasks app's record of the workspace (`tasks::model`,
kind `tasks.tracker`), written the one way every app is
([resources.md](resources.md)):

| Field | Type | Edited by |
|---|---|---|
| `number` | `u64`, read-only | `create`, from the tracker's counter |
| `title` | `String`, the label | field edit |
| `body` | `RichText`, a child resource | the rich-text editor's actions |
| `status` | Backlog, Todo, In progress, In review, Done, Cancelled | field edit; `place` from the board |
| `assignee` | nobody, a person or an agent | `assign` (never under a run) |
| `model` | a provider's model, or the agent's own | `choose-model` (never under a run) |
| `labels` | `Set<String>` | `add-label`; removing one is a field edit |
| `parent` | `Option<Id>`, read-only | `create`, `move-under` (refuses cycles) |
| `links` | `Collection<Link>` (app, record, kind, resource, label) | `link`, `unlink` |
| `area` | `Option<Area>`: the resource it works towards (app, kind, resource, label) | `plan` |
| `due_ms` | `Option<i64>`, a moment | `plan` |
| `priority` | `Option<u8>`, 1 (most urgent) to 4 | `plan` |
| `cost_budget` | `Option<u64>`, read-only: the most its work may cost | `plan` |
| `spend` | `Option<u64>`, read-only: what its runs recorded using | `record-spend` |
| `budget` | turn and total time ceilings | `set-execution-budget` |
| `comments` | `Collection<Message>`, the thread | `comment`, `send-message`, the run's reports |
| `run` | `Option<Run>`, read-only | the run verbs below |

History, undo, sync, search and permissions are the framework's. Nested issues
are a parent id on a flat collection, as notes' pages are; a parent's progress
is counted from its direct children, never stored. The thread is the
`comments` field, so a Commenter may post in it and nothing else.

Every issue in a tracker is read by everyone who reads the tracker: a read
decision reaches a record, never part of one. An issue that needs different
access is its own record, with the access every resource has; Tasks offers
no verb of its own for it.

Numbers come from a counter: increments merge, so the next number is always
past every issue a replica has seen. Two issues made concurrently on two
replicas can share a number; links and references carry the issue's id, the
number is how it reads.

## What an issue is

An issue is the whole of a task, and it holds nothing a person cannot read.
There is no second, app-private task store: what was asked is `title` and
`body`, the conversation around it is `comments` (a person's and an agent's
alike, each message saying which), what it may cost is `cost_budget`,
`budget` and `spend`, where it works towards is `area`, which device runs it
is `run.device`, and who answered for it is `run.starter`. Its goal is an
ordinary resource above it, named by address (`Area`), so a project or an
area is itself a resource with the access every resource has, never a field
of the issue that only Tasks understands.

`Run` holds what one run of an agent did and may do:

| Field | Meaning |
|---|---|
| `epoch` | the run's number on its issue, from 1 |
| `agent` | the agent working, as the door names it (`agent:<id>`) |
| `starter` | the person whose authority it acts under |
| `model` | the model it plans with, as the issue chose it when it started |
| `device` | the device it runs on, as its agent reports it (`run-on-device`) |
| `phase` | approved, accepted, planning, executing, waiting, done, failed, stopped |
| `started_ms`, `updated_ms` | when it started and last reported |
| `confirmation` | the action it waits on: the app, action, resource, digest of the exact arguments, the draft a person decides on and their answer |

Everything else the person sees of a run is the thread: its steps as
`from-run` messages, its questions as the confirmation above, and the
instruction and guidance it read as the `to-run` messages `incorporate` marks
as taken in. Nothing about a run is kept somewhere else, so history,
attention and the issue page all read the same fields.

## References

An issue's address is its item resource, `tasks.tracker/issues/<id>`
(`tasks::model::issue_resource`); `#12` is how it reads. Search finds an issue
by its number as an identity field, and by title, description and labels; a
hit, a reference and an agent all open it through the issue's `open` view
action. A link to any other resource is a `Link` (app, kind, resource,
label), the same address a `tilo_reference::ResourceRef` carries, with the
record of the app that keeps it (a page kept on its own); a link made before
links named a record has none, which means the app's own record of the
workspace, so old links need no migration.

Any app opens a reference the same way, with one action: the resource's
`open` in the app that keeps it (`ResourceRef::open_target`, see
[resources.md](resources.md) §3.5). An issue is `open` on
`tasks.tracker/issues/<id>` in `so.tilo.tasks`, admitted as the person under
their grants; the host launches Tasks when no window shows the tracker. `#12`
is the tracker's to resolve, never the shell's: the tracker's `open` takes it
as its anchor and shows the issue numbered 12 (`Tracker::by_number`), so an
agent or another app that holds only the text asks Tasks. The issue page shows
its links as reference chips (`ViewCx::reference`) that open each in the
record it names.

## Views

One window, three views over the same resource: a list that nests sub-issues
under their parent, a board with one column per status (dragging a card is one
`place` action: status and position), and an issue page with the markdown body
in the shared block editor, its sub-issues, links and thread. Its assignee
is picked among the room's people and agents by name
(`ViewCx::member_picker`, whom `@` offers in rich text). Its model
picker lists the models the person's providers offer, from their
`tilo_agent_models::OfferedModels` record: the session keeps it from
their provider settings, which hold endpoints and keys no app reads, and it
syncs to each of their devices like their accounts.

## Agents

Assigning an issue to an agent queues it; nothing runs until a person approves.
Every step is an action on the issue, so it is a permission in the one action
system and an entry in its history:

| Action | Who | What |
|---|---|---|
| `ask-agent` | people | an issue assigned to an agent whose run the asking starts (the "Tell an agent" composer) |
| `choose-model` | anyone who may edit | the model the issue's runs plan with, while no run is going |
| `approve-run` | people | starts run *n*+1 for the assigned agent, acting for the approver |
| `stop-run` | people | stops the run; what it already changed stays |
| `set-execution-budget` | people | turn and total time ceilings while no run is active |
| `send-message` | anyone who may comment | a message in the thread to the run going on |
| `report-run` | the run's agent | its phase, and progress posted in the thread |
| `incorporate` | the run's agent | the first *n* messages to the run, taken in |
| `ask-confirmation` | the run's agent | the draft of an action that asks first |
| `answer-confirmation` | people, on the host's sheet | confirm (with edits) or decline with a reason |
| `settle-confirmation` | the run's agent | the answer is spent once the action started |
| `run-on-device` | the run's agent | the device it works on, so its person sees where it runs |
| `record-spend` | the run's agent | what its runs have spent, in the workspace's units |
| `finish-run` | the run's agent | done goes to review, failed or stopped back to Todo |

The broker reads the tracker (`TrackerRuns`, served by the shell from the
room's committed record) and delivers each going run to its agent under the
native task lease, as each publication of that record is announced
(`policy_change`): the instruction is the issue's title and description, the
model is the one the run captured from the issue (the agent's own without),
the steering is the thread's messages to the run, and a run the tracker no
longer holds as going is cancelled. The agent writes the run's reports, questions and
end itself, through the tracker's actions above, as children of its run.
Every change the run makes carries the run's own group
(`runtime_collab::task_run_group`), so history lists each run of an issue
as one run of its agent and undoes it whole.

Each of the agent's actions, the tracker's included, is admitted as the agent
working on the issue, acting for the run's approver: the room's grants
naming the agent or the issue decide, the most specific first, bounded by
the approver's live grants ([permissions.md](permissions.md) §4), with
every message to the run read first. An action that asks first waits for
the person's answer on the issue, and a confirmation admits only the
arguments asked about, when the draft the person saw is the one those
arguments render. The issue page shows the question with one Answer
button; the answer it sends goes through the session's door, and the host
puts the question, with its draft, to the run's person on its own
confirmation sheet: Allow once, Allow for the issue (by its title), or
Deny. The run's person is also told with an ordinary notification, whose
Answer opens the same sheet; nothing raises the sheet on its own. The
tracker records the answer they chose there, never the app's.
Allow once grants nothing. Allow for the issue first records, as the
person, an answer grant naming the issue for that one action
(`agent_answers::answer_grant`), so its runs take that action without
asking again; workspace settings list it with every other grant, the
issue named by the title Tasks publishes for it, with revoke.

A run may act in an application nobody has open: the broker launches it
through the workspace launcher under the run's own durable custody, bound to
the run, its approver and the messages it took in, and admits the action
against the freshly published contract only. Stop and a revoked grant fence
both the launch and the action after it. A run's turn and total time
ceilings are the issue's `set-execution-budget`.

## The composer

"Tell an agent" asks through the tracker: work becomes an issue whose run
the asking starts (`ask-agent`), a message to it is `send-message` and Stop is
`stop-run`. The composer follows the run on the room's tracker, read by the
shell from the committed record whenever the room's owner publishes, so it
says the run ended, and how, with the agent's last words, when the tracker
does.

## Migration

Tasks once lived as records in the workspace's room document. Once a
session, when the person opens a workspace, the shell reads the tasks the
room still keeps (`room_task_import`) and hands those the tracker has not
imported to its `import`, as the person. Each becomes an issue under an id
derived from the task's, so two devices importing at once make one issue,
and an imported task is never imported again. A task's run does not move
with it: an agent's task waits in Todo for a person to approve it again.
Nothing else reads or writes the room's task records: the broker runs only
issues, and a notification handed to an agent is asked through the tracker,
as the composer asks.
