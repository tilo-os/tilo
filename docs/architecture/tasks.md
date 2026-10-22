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
| `comments` | `Collection<Message>`, the thread | `comment`, `send-message`, the run's reports |
| `run` | `Option<Run>`, read-only | the run verbs below |

Beside its issues the tracker holds `private`, the private issues it lists by
id alone (below).

History, undo, sync, search and permissions are the framework's. Nested issues
are a parent id on a flat collection, as notes' pages are; a parent's progress
is counted from its direct children, never stored. The thread is the
`comments` field, so a Commenter may post in it and nothing else.

Numbers come from a counter: increments merge, so the next number is always
past every issue a replica has seen. Two issues made concurrently on two
replicas can share a number; links and references carry the issue's id, the
number is how it reads.

## Private issues

A read decision reaches a record, never part of one, so a private issue
lives in a record of its own: a tracker holding just that issue, a child
record of the workspace's tracker (`tasks::model::private`). The tracker
lists it in `private` by its id alone; its kind and whether it is in the
trash are all a reference says. The room keeps that record to its readers
with one grant ([permissions.md](permissions.md) §5.3): the person who made
it private and the people and agents they named. Nobody else's device is
given its key, and every door denies it to them; an agent reads it only when
it is named itself and its person reads it too, so a run of a private issue
is admitted for a named agent and an approver who reads it.

Making an issue private is one action, the tracker's `take` on its
`private` container ("Make private" on the issue page, where the room's
members are offered by name and agents by id): the room is asked to keep
the issue's record to the readers with its own `grant`, then the issue moves
whole into it, its run aside (a run is approved there by someone who reads
it), and leaves the tracker in the same step. Nothing is ever half private:
a refused grant changes nothing, and an issue with a run going or with
sub-issues stays where it is. The private issue keeps its id and number, so
its address and every reference to it stay; its record is the tracker's
record's child of that id (`tasks::model::private::private_record`). What
the tracker held before stays in its history.

For its readers a private issue shows among the others: the list, the board
and search read the workspace's tracker together with every private tracker
the device reads, and the issue's page is the same page over its own
tracker, so editing it, its thread and its runs are actions on that record.
Others see nothing of it. The runs the broker is given include those of
every private tracker the device reads, each only while its agent, acting
for the person who approved it, reads it too, and the agent reports to the
tracker that keeps the issue.

Making an issue public again, changing its readers and making a new issue
private from the start are not offered yet: a new private issue is written
in the workspace's tracker first.

## References

An issue's address is its item resource, `tasks.tracker/issues/<id>`
(`tasks::model::issue_resource`); `#12` is how it reads. Search finds an issue
by its number as an identity field, and by title, description and labels; a
hit, a reference and an agent all open it through the issue's `open` view
action. A link to any other resource is a `Link` (app, kind, resource,
label), the same address a `tilo_reference::ResourceRef` carries, with the
record of the app that keeps it (a private issue's tracker, a page kept on its
own); a link made before links named a record has none, which means the
app's own record of the workspace, so old links need no migration.

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
in the shared block editor, its sub-issues, links and thread.

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
| `answer-confirmation` | people | confirm (with edits) or decline with a reason |
| `settle-confirmation` | the run's agent | the answer is spent once the action started |
| `finish-run` | the run's agent | done goes to review, failed or stopped back to Todo |

The broker reads the tracker (`TrackerRuns`, served by the shell from the
room's committed record) and delivers each going run to its agent under the
native task lease: the instruction is the issue's title and description, the
model is the one the run captured from the issue (the agent's own without),
the steering is the thread's messages to the run, and a run the tracker no
longer holds as going is cancelled. The agent writes the run's reports, questions and
end itself, through the tracker's actions above, as children of its run.

Each of the agent's actions, the tracker's included, is admitted as the agent
acting for the run's approver: bounded by the approver's live grants, the
agent's grants and the issue's own grant set, with every message to the run
read first. An action that asks first waits for the person's answer on the
issue, and a confirmation admits only the arguments asked about, when the
draft the person saw is the one those arguments render.

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
