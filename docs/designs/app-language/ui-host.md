# The UI host: apps that are basically free

**Goal.** An open tilolang app costs under 5 MB of its own memory, no CPU while idle, and shows
its first frame within 50 ms. A slow device works fine with a large workspace.

**Why apps are expensive today.** Every app process links the whole UI toolkit (widgets,
layout, text, fonts, rendering): about 50 MB before its data.

## 1. Three parts

| Part | Is | Holds | One per |
|---|---|---|---|
| **UI host** | the workspace's render service, extended | widgets, layout, text, fonts, rendering, input, accessibility | workspace |
| **tilolang host** | a small process without the toolkit (crate `tilolang-host`) | the apps' wasm instances, their records, undo and history | workspace and author |
| **App instance** | wasm in a tilolang host | its module and memory | open app |

- An app sends **component patches**, not pixels. The UI host applies them to that app's
  widgets and draws them with one shared font set, glyph cache and GPU device.
- Input goes back as events. Typing happens in the UI host's text field, so it shows on the
  next frame without a round trip; the edit then goes to the app's record.
- Apps by one author in one workspace share a tilolang host, isolated as wasm instances.
  Different authors get different processes, so a stranger's code never shares memory with
  yours. An author is the app's verified publisher, the person for their own builds, or the
  task for an agent's programs.

## 2. What stays

- **Identity, custody, sandbox.** Each instance gets its own launch record with its app's
  creation record and ticket. The launcher opens each instance's connections (broker, identity,
  window) with that app's own custody and hands them over. Nothing trusts a host's claim about
  which app it is.
- **Windows belong to apps.** Each app window keeps its launch identity for its surface and
  accessibility, but opens no broker connection: the window is presentation, the instance in
  the tilolang host holds the data and publishes the actions.
- Native Rust apps don't change. They migrate by becoming tilolang apps.

## 3. Patches are checked

A patch is untrusted until checked: components, props and events must match the component
catalogue (`components.json`); sizes are bounded; a patch can only touch its own app's widgets.
A bad patch is refused and only that app shows an error. A crashed tilolang host takes down only
that author's apps, which show their last frame and come back on restart.

## 4. Thin mode

Views are functions of resources, so they can run where the data is. On a slow device, the
tilolang host runs on the workspace's host and only patches for what's visible travel to the
device. Typing still applies locally first. Encrypted workspaces whose host can't read them run
locally.

## 5. Steps

| Step | What | State |
|---|---|---|
| 0 | The commit path (broker client, managed sender) in crate `collaboration`, without the toolkit | done |
| 1 | The patch protocol; the app half without the toolkit; the UI half (`tilo-ui-host`) | done, in one process |
| 2a | Undo, history steps and save state in `collaboration`, shared with native apps | done |
| 2b | The tilolang host process: launch kind, sandbox, instances, `RoomRecord` holding each record | process done; the shell starting hosts and handing them instances is in progress |
| 2c / 3 | The UI host draws every app's windows | next |
| 4 | Measure against the goal, including a slow device (2 cores, 4 GB) | partly: 2.7 MiB per extra instance, first frame 2.4 ms median |
| 5 | Thin mode | later |

Decisions made on the way:
- The record store (`RecordStore`) stays with native apps. The tilolang host holds records
  through `RoomRecord`, using the same undo log, history and save code.
- Rust visibility isn't the security boundary inside an app's process; the broker checks every
  route and commit. Functions public only for ui-core are hidden from docs.
- A window's pixel buffers (about 16 MiB at 1280×800) belong to the UI host and don't count
  towards an app's memory.

## 6. Open

- How the compositor accepts surfaces the UI host presents for several apps: per-surface
  attribution, only from a render service it started.
- The UI host's own memory grows with open windows, not with apps.
- Which view state lives where: lens state (scroll, selection) in the UI host, window state
  that views read in the app.
