# Session replay

A recording of what happened lets whoever reads it step through the same
states its author saw. Tests, agents and anyone following a past session read
the same recording.

Nothing new records anything. A resource is already an action log: every
change names its author, its time and the action that made it, and History
already rebuilds any past state of it and draws it with the app's own view
(`resources.md`, `tilo_resource::history`). A recording is a stretch of that
log; replaying it is History over that stretch.

## What a recording holds

One recording covers one record over a window of time:

- the record's state at the start of the window, as one change;
- each change made to it during the window, in causal order, each with its
  original author (the change's actor), time and action entry.

Nothing else is in it. In particular it holds no change from before the
window, no state of any other record, no presence, pointer or pixels.

The original record cannot be handed out instead. An Automerge change graph
cannot be served in part (`workspace.md`): sharing the record would share all of
its history and every later change. So the recording is a new document built
from the stretch (`tilo_resource::recording`): its first change writes the
start state, and every later change is the original change's effect, read as
Automerge's own diff between the two frontiers and written with the
original's actor, timestamp and action entry. A gesture's continuation is
written as the full entry it stands for, since the gesture may have started
before the window. The record's own identity (`tilo:record`, its home
workspace) is left out: the recording is not that record. After writing, the
recording's final state is compared with the record's state at the end of the
window in full; any difference is an error, never a silently different
recording.

The recording is therefore a resource of the recorded app's kind whose whole
history is the window. Its steps are its History entries, and its state at
step *n* is History's state at that entry.

Apps that declare their windows withheld from captures
(`ComponentBehaviour::capture_protected`: passwords, authenticators) are
never recorded. Each desktop reads that declaration from the app's own
windows, the native one also as an app in its own process asks for it, and
the recorder checks nothing else. Recordings are bounded in steps and bytes
(`recording::Limits`). A window with more steps than the bound starts later,
so the last steps before its end are always there; a recording larger
than the byte bound is not offered.

## Where recordings come from

A problem report is a message in the workspace's #feedback channel (Chat's
`report_problem`) and carries no recording: there is no step yet that cuts one
for a report. A test or an agent cuts one with `tilo_resource::recording` and
opens it as below.

## How replay steps

Replay opens the recorded app with the recording, the way any app opens
content another app holds (`collab_types::app_actions::ApplicationLaunch`): the
workspace reads the bytes through Chat's own content scope, verifies them
against their hash and hands them to the app as a read-only file. A
recording's bytes start with a header that says what they are
(`tilo_content::RECORDING`), so every app built on the framework opens one:

- in a private window, whose profile is thrown away with it and whose
  record reaches no other window, device or member;
- on a record of its own holding exactly the recording's changes;
- read-only: nothing in the window can change it;
- with History open at the recording's first step.

Stepping is History's own timeline, drawn by the app's own view of each
past state. Nothing in replay is specific to feedback or to an app.

## Tests, agents, following

- A test cuts a recording of a resource it drove and asserts the states its
  steps replay to (`tilo_resource::recording`).
- An agent reads a recording's steps and the state at each through the same
  module, or lists them through the replay window's History.
- Following a past session is stepping the same History forward at the pace
  of the steps' times.
- Looking at a past state is a read: it changes what this viewer sees, not
  the record. History's looking actions (listing, viewing at an entry,
  comparing, opening at a change) need only read access, so a read-only
  member, the read-only replay window and an agent all step through them;
  restoring and naming write and need write access.

## Checked against the principles

- **Resources are action logs.** The recording is a stretch of the record's own
  log on a start snapshot; nothing records in parallel.
- **One mechanism.** Recording reads History's data; opening it is the launch
  every app uses to open content another app holds; replay is History of a
  record. No new transport, stream or viewer.
- **Every button is an action.** Stepping is History's actions.
- **Privacy.** The window excludes earlier history and every other record;
  protected apps are never recorded; a replay opens only in a private window,
  so it never becomes a record anyone else holds.
