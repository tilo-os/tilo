# Seeing what a task did

An agent may work on anything in the workspace. What makes that acceptable is that the person
can always see what it did, where it did it, and take it back. This is the mechanism behind
shell.md §5. It is independent of which shell is running.

## 1. What exists

- Every change a run makes is stamped with the run's group (`runtime_collab::task_run_group`),
  so history lists a run as one unit and `undo task` reverses it whole (tasks.md, "Agents").
- `record-step` keeps each program the run ran with its changes; the issue's thread shows them
  as rows that undo.
- tilolang reflection reads a resource's versions: `created(r)`, `updated(r)`, `history(r)`
  (who, when, which action).

What's missing is the link from a version to its task, a notion of "seen", and anything that
draws it.

## 2. The record

A version names the task run that made it:

```
type Version { by: Person | Agent, at: Time, action: Text, task: TaskRun?, kind: .created | .changed | .removed, fields: Set<Text> }
```

`task` comes from the change's group stamp; the runtime fills it, nothing new is stored.
Removals are versions too, so a removed record stays answerable (`history(r)` of a removed `r`
says who removed it, in which run).

Two queries cover the presentation:

```
changes(of: run)                     // every version the run made, by step, oldest first
unseen(r)                            // r's versions newer than the person's seen mark for r
```

**Seen marks** live in the person's own record (`records: .own`): per resource, the newest
version they have seen. Showing a resource on screen does not set it; the person does
(§4).

## 3. Marks in place, with no app code

The UI host already knows which resource each part of a view is bound to: `std.list` and
`std.table` rows are references to items, `ui.TextInput(value: n.title)` binds a field, every
window shows a resource. The host draws marks from `unseen(r)` at those points:

| Where | Mark |
|---|---|
| a list or table row | new / changed, with who: the agent's face, "Agent · run 3" |
| a removed item | a struck row in its old place, "removed by Agent", with Undo |
| a bound field | the field tinted, the previous value on hover |
| a window | its title carries the count of unseen changes and who made them |

Apps write nothing for this. A view may place the mark itself (`ui.ChangeMark(r)`) when the
default spot is wrong; the host then skips its own.

Every mark activates to the step that made it: the run's log opens at that step, with Undo for
the step and for the run.

## 4. Seen

A mark stays until the person deals with it, never on a timer:

- **Reviewing the run:** the run's log has *Mark all seen*, and finishing review of the issue
  (tasks.md, done goes to review) does the same. This is the normal path after an agent run.
- **Touching the resource:** editing it, or opening it in a window of its own, marks what it
  shows as seen.
- **Undo** removes the marks of what it undid.

Changes by other people use the same marks, off by default per workspace; changes by agents
are always marked.

## 5. Watching and contained

Each run has a presentation, chosen by the person who started it and switchable while it runs.
It's kept in their own record; the agent is never told which.

**Contained** (default for runs started from chat or the tracker): nothing in the person's
arrangement changes. The run's log shows each step as the program it ran, then a live embedded
view of every resource it touched (`std.view(r)` with marks), so the log is where the work is
read and undone.

**Watching** (default for asks typed on the canvas or a window): the shell shows the work where
it happens.

- What the run creates appears near where the person asked: the shell places a reference
  to it beside the ask's anchor, as if the person had activated it (shell.md §3). It is the
  person's arrangement change, attributed to the ask, undone with the run.
- The run's presence moves to what it's working on, like a collaborator's: its face on the
  window or row it is reading or changing. Presence comes from the steps; the agent does not
  report it.
- Things already shown that it changes are marked in place as in §3, live.

On a phone, watching is a notification per finished step whose activation shows the result.

## 6. Order

1. `Version.task` and `kind`, `changes(of:)`, `unseen(r)`, seen marks in the own record.
   Tested with a run that creates, changes and removes records in two apps.
2. Host-drawn marks on list rows, bound fields and windows; activation opens the step. Pixel
   tests on Notes and Tasks with a scripted run.
3. Contained: the run log's steps embed live views of what they touched.
4. Watching: placement beside the ask, agent presence from steps. Built on the new compositor's
   window policy, with the shell work (shell.md §8).

Steps 1–3 need no shell work.

## 7. Open

- Whether people's changes are marked by default in shared workspaces, or only agents' (§4
  says agents only).
