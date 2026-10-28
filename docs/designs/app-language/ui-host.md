# The UI host: apps that are basically free

**Goal.** An open tilolang app costs almost nothing: under 5 MB of its own memory, zero CPU while
idle, and a first frame within 50 ms. A slow device connected to a workspace with a host is fine
for everything, even very large workspaces.

**Where the cost is today.** Every app process links the whole toolkit (ui-core): widgets,
layout, text shaping, font and glyph caches, scene recording and clipboard threads. That's about
50 MB per app before its data.

The room's render service already draws for every app in a room
(`docs/architecture/app-rendering.md`): it holds one GPU device, warm pipelines and one glyph
cache. That design names its own step 3: "the system composes apps' views". This is that step,
for tilolang apps.

## 1. The model

| Part | Process | Holds | Per |
|---|---|---|---|
| **UI host** | the room's render service, extended | widget trees, layout, text, fonts and glyphs, scenes, rasterizing, input, IME, clipboard, accessibility tree | room (as today) |
| **Tilolang host** | `apps/tilolang` without its window code: no toolkit | wasm instances of apps, the tilolang runtime, store and index clients | room and author |
| **App** | a wasm instance in a tilolang host | its module and its wasm memory | app instance |

- **An app sends component patches, not scenes.** These are the described-component patches the
  tilolang runtime already produces (`StructureChange::Described`, keyed slots, prop sets), in
  `tilo-ui-graph`'s component format. The generic adapter (`apps/tilolang/src/adapter.rs`) moves
  into the UI host.
- **The UI host applies patches to that app's widget subtree.** It lays out, records and
  rasterizes exactly as the render service does today. One font set, glyph atlas, theme and GPU
  device serve every app in the room.
- **Input goes back as events.** Widget events become action references or view-state updates,
  and bindings become field edits. Text editing happens in the UI host's `TextInput`, so typing
  shows on the next frame without a round trip to the app. Splices go to the app's store as
  bindings.
- **No toolkit in the tilolang host.** It links the tilolang runtime, the store and index clients and
  wasmi (later Cranelift), and is a few MB. One per room and author: apps by one author in one
  room share a process, isolated as wasm instances. Different authors get different processes,
  so a hostile app shares no address space with another author's (Spectre).

**Names.** `tilo-app-host` (crates/packaging/app-host) is the package daemon and the "shared app
runtime" is native apps' shared library; neither is the tilolang host.

## 2. What stays exactly as it is

- **Per-app identity, custody and the sandbox.**
  - Each app instance is launched by the launcher under its own app id, with launch custody,
    grants and broker routes, as today.
  - The tilolang host is sandboxed (Landlock, namespaces, seccomp) like any app process.
  - Custody is recorded per instance, not per process.
- **Windows belong to apps.** The UI host creates one Wayland surface per app window. The
  compositor attributes it to that app's id through the broker, which already decides which
  service draws which app from the launcher's attribution, never from what an app says.
  Admission, window rules, the window bar and History all see the app as before.
- **The room is the boundary.** As today, apps in a room with strangers never share a UI host with
  another room.
- **Native Rust apps are unchanged:** they keep their own surface and send scenes. They migrate by
  becoming tilolang apps.

## 2.1 Decisions from building step 1

Step 1 is built: the validated patch protocol, the app half without ui-core
(`tilolang_tilo::instance`) and the UI half (`tilo-ui-host`), still in one process per app.

Measured in a harness with 30 apps:

| Measure | Result |
|---|---|
| App half | 2.2–3.9 MiB |
| UI half per window, before its pixel buffers | 0.1–0.6 MiB |
| Idle, all 30 apps for 5 s | one context switch |
| First frame | 1–31 ms |

Building it showed four gaps. They are decided as follows:

1. **Custody, surfaces and accessibility: the launcher opens per-instance connections.** For each
   app instance, the launcher opens that app's broker, Wayland and accessibility connections
   with the app's own launch custody, exactly as for a launched process, and passes the file
   descriptors to the tilolang host (broker) and the UI host (Wayland, accessibility). Every connection
   is attributed as today. There is no new admission model and no compositor change, and the
   compositor never trusts a host's claim about which app a surface is.
2. **The framework root splits by data and presentation.**
   - **Tilolang host:** undo logs, history records, save state, actions, the unreadable-record
     state.
   - **UI host:** the palette, the Ask bar and the History views, which show the tilolang host's
     records.
3. **The commit path becomes a crate of its own, without the UI.**
   - **What the tilolang host needs:** tilolang's app half (`tilolang_tilo`) already has no
     toolkit. It plans every change as a recorded action (`DynAction`) and follows the room record
     through `TiloStore::sync_from`. What it lacks is the path that commits those actions and
     brings the record's changes back. Today `apps/tilolang` borrows that path from ui-core.
   - **What moves:** the broker client in `ui_core::collaboration` (routes, transport, committed
     reads, signing, invocation updates, presence transport), the record store it commits to,
     and the managed sender with its private storage, whole. The sender's constructors stay
     private: custody is never loosened to make a cut easier.
   - **What stays in ui-core:** the door's widget side (`bound_dispatch`, `invocation_scope`'s
     UI state), participants and content loading state. Widgets live in the UI host; an event
     there becomes an action the tilolang host executes and commits.
   - **Where to:** the crate `collaboration` (crates/collab/collaboration), re-exported under the
     old `ui_core` paths. The broker's ui-core dependencies become small interfaces in the crate
     (document custody, native launch errors and connections, component bindings, record read
     errors) that ui-core implements.
   - **Done when:** `tilolang_tilo::instance` commits through the crate, and its dependency tree
     has no ui-core and no tilo-host.
   - **Why it's worth doing anyway:** thin mode, browser workers and the convergence log
     (`convergence.md` §9) all need it. Native apps keep working through ui-core on top of it.
   - **Order:** this is the prerequisite for step 2.
4. **Pixels are the UI host's.** The memory target excludes a window's pixel buffers: about
   16 MiB at 1280×800, the same for any app, native or not.

## 3. Safety at the patch boundary

A patch is hostile until validated, as scenes are today:
- **Valid by type:** components, props, slots and events must match the toolkit's descriptions
  (`components.json`).
- **Bounded:** sizes and counts are limited, and each app's subtree has its own budget.
- **Only its own subtree:** a patch can't name another app's nodes.
- **Errors contained:** a bad patch is refused, and that app's window shows the platform's error
  state. The UI host and the other apps keep running. A crash in a tilolang host takes only that
  author's apps in that room. Their windows show their last frame and come back when it
  restarts, as the render service does today.

## 4. Thin mode for slow devices and large workspaces

Views are pure functions of resources, so they can run where the data is:
- **Thin mode:** the tilolang host runs **on the workspace's host**, next to the full data and index.
  Only patches for what's visible stream to the device. The device's UI host renders them at full
  speed.
- **Typing is local:** text edits apply in the device's UI host at once, then go to the store.
- **The device holds** its UI host, its open windows' widget trees and caches of what it showed.
  Nothing else.
- **Mode is chosen per device and workspace** by what the device can do: local when it has the
  data and the cores, thin when it doesn't. Encrypted rooms whose host can't read content run
  locally.

## 5. What this removes

- the per-app toolkit, fonts, glyph caches and GPU and render workers;
- per-app clipboard threads waking on every frame;
- per-app action-surface publication on every key: the UI host publishes what its widgets offer,
  and the tilolang host publishes the app's actions;
- the per-launch cost of loading the toolkit: an app starts as an instance in a warm process.

## 6. Steps

0. **The store client crate** (§2.1, point 3).
1. **The UI host.** Extend the render service with the toolkit, the generic adapter and a patch
   protocol (patches, events, bindings, focus, IME) beside its scene protocol. Move tilolang's
   window code (`apps/tilolang`) into it: shell composition, `std` frames as they are today, and
   lists.
2. **The tilolang host.** A launcher-started process per room and author, running the tilolang
   runtime with instances per app, with custody, grants and broker routes per instance. Hot
   reload and tiered execution are per instance.
3. **Surfaces per app window from the UI host,** attributed through the broker. The window bar,
   History, actions and accessibility as before.
4. **Measure against the targets:**
   - marginal memory per app under 5 MB;
   - zero idle wakeups;
   - first frame under 50 ms;
   - keystroke-to-screen with no app round trip;
   - 50 open apps comfortable in 4 GB.

   A low-end profile (2 slow cores, 4 GB) runs the same scripts.
5. **Thin mode:** the tilolang host on the workspace host, with patches over the room's connection.

## 7. Open

- **How the compositor trusts the UI host's surfaces.** The UI host presents surfaces under
  several app ids. The broker vouches for each, and the compositor should accept per-surface
  attribution only from a render service it started.
- **The UI host's own memory.** It now holds every window's widget tree in the room. Its budget
  grows with open windows, not with apps.
- **Which widget state lives where.** Lens state (scroll, selection, caret) is in the UI host;
  window state that views read is in the app. The split follows `workspace-model.md`.
