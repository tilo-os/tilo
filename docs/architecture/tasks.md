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
| `assignee` | nobody, a person or an agent | field edit |
| `labels` | `Set<String>` | `add-label`; removing one is a field edit |
| `parent` | `Option<Id>`, read-only | `create`, `move-under` (refuses cycles) |
| `links` | `Collection<Link>` (app, kind, resource, label) | `link`, `unlink` |
| `comments` | `Collection<Message>`, the thread | `comment`, `send-message`, the run's reports |
| `run` | `Option<Run>`, read-only | the run verbs below |

History, undo, sync, search and permissions are the framework's. Nested issues
are a parent id on a flat collection, as notes' pages are; a parent's progress
is counted from its direct children, never stored. The thread is the
`comments` field, so a Commenter may post in it and nothing else.

Numbers come from a counter: increments merge, so the next number is always
past every issue a replica has seen. Two issues made concurrently on two
replicas can share a number; links and references carry the issue's id, the
number is how it reads.

## References

An issue's address is its item resource, `tasks.tracker/issues/<id>`
(`tasks::model::issue_resource`); `#12` is how it reads. Search finds an issue
by its number as an identity field, and by title, description and labels; a
hit, a reference and an agent all open it through the issue's `open` view
action. A link to any other resource is a `Link` (app, kind, resource,
label), the same address a `tilo_reference::ResourceRef` carries.

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
| `approve-run` | people | starts run *n*+1 for the assigned agent, acting for the approver |
| `stop-run` | people | fences the run; what it already changed stays |
| `set-execution-budget` | people | turn and total time ceilings while no run is active |
| `send-message` | anyone who may comment | a message in the thread addressed to the current run |
| `report-run` | agents | the run's phase and progress, as thread messages |
| `ask-confirmation` | agents | records the draft of an action that asks first |
| `answer-confirmation` | people | confirms (with edits) or declines with a reason |
| `finish-run` | agents | ends the run: done, failed or stopped |

The agent's own actions on other resources are admitted as before: as the agent
acting for the run's approver, bounded by the approver's live grants, the
agent's grants and the run's frozen scope (the issue's links narrow it). The
runner reads its instruction from the issue's title and body, and the thread's
messages addressed to its run, never another issue's.

## Migration

Tasks once lived as records in the workspace's room document. The Tasks app
reads them once through the workspace's task actions and imports each as an
issue under an id derived from the task's, so two devices importing at once
make one issue, and an imported task is never imported again.
