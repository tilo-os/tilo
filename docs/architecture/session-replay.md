# Session replay

A report can carry a recording of what happened, so whoever reads it can step
through the same states the reporter saw. Tests, agents and anyone following a
past session read the same recording.

Nothing new records anything. A resource is already an action log: every
change names its author, its time and the action that made it, and History
already rebuilds any past state of it and draws it with the app's own view
(`resources.md`, `tilo_resource::history`). A recording is a stretch of that
log; replaying it is History over that stretch.

## What a recording holds

One recording covers one record, the one the window the report is about
shows, over a window of time that ends when the report is written:

- the record's state at the start of the window, as one change;
- each change made to it during the window, in causal order, each with its
  original author (the change's actor), time and action entry.

Nothing else is in it. In particular it holds no change from before the
window, no state of any other record, no presence, pointer or pixels.

The original record cannot be handed out instead. An Automerge change graph
cannot be served in part (`rooms.md`): sharing the record would share all of
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

Apps that withhold themselves from captures (`capture_protected`: passwords,
authenticators) are never recorded. Recordings are bounded in steps and bytes
(`recording::Limits`). A window with more steps than the bound starts later,
so the last steps before the report are always there; a recording larger
than the byte bound is not offered.

## How a person includes one

The composer offers "Include a recording" only when the window the report is
about shows a record of an app that may be recorded. It starts off, like the
screenshot. The person sees, before they send, exactly what it includes: the
app, the time range and the number of changes, and that whoever can read the
report can see the record's whole state at the start of the range and every
change during it, with who made it.

The record is read as its workspace committed it, the state History and
Activity read. Turning it on builds the recording on the device when the
report is reviewed; nothing leaves it until the report is sent. Review covers the report's text and picks as before; the
recording is shown as the scope above, since its content is the record
itself.

## How it is shared

Like the report's picture. The submission carries a `RecordingRef`: the
content hash of the recording's bytes, the app and kind it belongs to, the
time range and the number of steps. The bytes are the report's content in the
destination workspace's Chat record: the reporter's device keeps them, and
whoever may read that Chat asks the record's route for them and gets them on
it, end-to-end encrypted like every other content the record references. The
workspace's host keeps its own copy, as it keeps report pictures, and the
reporter's receipt waits until it does. There is no other transport and no
server-side plaintext copy: the host keeps the content because it is a member
of the workspace.

## How replay steps

The report's message in Chat shows the recording's scope and a Replay
button. The report item publishes the same step as its `replay-recording`
action, so a script, an agent and assistive technology replay through the
same door.

Replay opens the recorded app with the recording, the way any app opens
content another app holds (`tilo_actions::remote::ApplicationLaunch`): the
workspace reads the bytes through Chat's own content scope, verifies them
against their hash and hands them to the app as a read-only file. A
recording's bytes start with a header that says what they are
(`tilo_content::RECORDING`), so every app built on the framework opens one:

- in a private window, whose profile is thrown away with it and whose
  record reaches no other window, device or collaborator;
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

## Checked against the principles

- **Resources are action logs.** The recording is a stretch of the record's own
  log on a start snapshot; nothing records in parallel.
- **One mechanism.** Recording reads History's data; sharing is the record
  content path report pictures already take; opening it is the launch every
  app uses to open content another app holds; replay is History of a
  record. No new transport, stream or viewer.
- **Every button is an action.** Including a recording is a field of the
  report the composer's actions set; Replay is a Chat action; stepping is
  History's actions.
- **Privacy.** Off by default, included only when the person chooses it, with
  its scope shown before sending; the window excludes earlier history and every
  other record; protected apps are never recorded; a replay opens only in a
  private window, so it never becomes a record anyone else holds.
- **End-to-end encryption.** The bytes reach only members of the destination
  workspace, its host as a member included.
- **Causes, not symptoms.** Content that has not arrived yet is waited for by
  the content route's own arrival signal, never polled.
