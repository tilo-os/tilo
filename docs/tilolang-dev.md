# Iterating on a tilolang app

Two loops:

- **The app loop.** Save a `.tl` file. The running app shows the change in about
  50 ms, with its state: its resources, its view state, the scroll,
  the selection and the open item.
- **The runtime loop.** Change the runtime (`apps/tilolang`, `tilolang-tilo`,
  `tilolang-runtime`) and build it. The running app restarts on the new runtime,
  almost all of it the build. Its resources are still there.

There are two ways to run them:

- **On this desktop.** `tilolang dev DIR` runs the package as an ordinary window, with no
  Tilo session: it is the developer's own app, and its records are its local store. This
  is the everyday loop.
- **In a real session.** `tilolang dev DIR --session SESSION` develops the package inside
  a running nested session, where it runs as an installed app: launched by the launcher,
  in its sandbox, under its own id and custody. Use it to test a change against a real
  session.

## On this desktop

Build the tool with the workspace's `fast` profile, the one it names for the desktop:

```sh
cargo build --profile fast -p tilolang-cli --bin tilolang
```

and start the loop:

```sh
target/fast/tilolang dev apps/tl/tasks --search apps/tl
```

The first start builds `tilolang-app` from the checkout in the same profile (an ordinary
build; a thin build runs only against a session's runtime library) and opens the package's
window. `--runtime BIN` runs a binary you built yourself instead, and a new build of it
restarts the window on it.

What the loop counts as a save: anything that changes what a build reads. That is every
`.tl` file of the package and of the packages it uses, the translations in their
`locales/`, and entries added, removed or renamed anywhere below their directories, a
whole package directory moved in included (the loop watches the directory above each
package, so a package replaced whole is taken again). Then it compiles once: the events of
one save, and of one build writing a file in steps, are gathered for a few tens of
milliseconds first.

A `--runtime BIN` build counts too, and the loop starts the window again only once the new
program is whole: a build that copies the binary in place, or replaces its directory, is
taken as soon as the file has stopped growing and is executable (up to 60 seconds). The
window comes back on its checkpoint: the document, the route, the view state and the
caret. The old process is stopped first, with time to flush.

If the sources use a type this build of the tool does not know (`E013 unknown type`) and
the checkout's own sources are newer than the tool, the loop says so instead of leaving the
error to look like a mistake in the package: rebuild the tool and start `tilolang dev`
again.

The profile is not a detail of this loop: it compiles each saved package, interprets it and
draws every frame of its window, and an unoptimised build of it does each of those several
times slower. `fast` is the workspace's answer for a local build that must run at
near-release speed.

The loop is a directory protocol (`tilo_app_module`, `TILO_APP_MODULE_UPDATES`): the tool
publishes each build as a numbered generation and names the current one in a `current`
file; the app watches that file. Nothing else can reach the window. A generation holds the
compiled bundle itself: a local build stages no payload, packs no `.tapp` and signs nothing,
because nothing installs it.

- The app is a plain process the tool starts and stops: it has no session, no launcher and
  no sandbox around it.
- Its record is the framework's local store (`$XDG_DATA_HOME/tilo`), so resources persist
  across saves, across a process restart and across later `tilolang dev` runs.
- `Ctrl-C` stops the tool and the window. Closing the window stops the loop.

## In a real session

You need a nested-session build (`dist/dev/nested-session-build.sh
target/nested-session`, which `dist/dev/nested-session.sh` builds) and the tool, built
with the nested session's profile:

```sh
cargo build --profile nested --config 'profile.nested.inherits="release"' \
    --config profile.nested.lto=false --config profile.nested.codegen-units=16 \
    --config profile.nested.incremental=true -p tilolang-cli --bin tilolang
```

The examples below call the result `tilolang` (`target/nested/tilolang`).

```sh
# Start a session and the app in it; from then on, every save reaches the app.
tilolang run apps/tl/notes --search apps/tl --headless --software
```

`tilolang run` opens the app once the session's catalogue offers it (`applications
<id>`): it asks the shell for the applications listing and the shell holds the answer
until the listing has settled or changed. If the run fails at any point after the session
started, the tool stops the session: the shell, its apps and its services.

To work in a session that is already running, attach to it:

```sh
dist/dev/nested-session.sh --headless --software --state /tmp/s --ready /tmp/s.json &
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json
```

`--session` takes the `--ready` file, the session's runtime directory or its control
socket.

There, on every save `tilolang dev`:

1. compiles the package in-process (tens of milliseconds);
2. on an error, prints the diagnostics and stops: the app keeps running the last good
   build;
3. signs the build into a `.tapp` and installs it with `tilo-install`, like any
   install;
4. says in one line what the running app does with it.

## The app loop

Both ways of running take a new build the same way, in one of two ways:

- **In place.** This is the usual case. Resources live in the workspace record and view state lives
  in the runtime, both keyed by declaration id. Scroll, selection and the open item live in the
  window. So the new module takes them over: the views are opened again on the same windows, under
  the same shell.
- **Starting over.** If the new build cannot read what the old one wrote, the tool says why, for
  example `type Note: the field \`done\` changed from Bool to Int`. The app then starts over on the
  new build and the workspace record, as its next start would: every resource is still there. View
  state and the open item start fresh. Nothing is deleted. A value of the old type stays in the
  record until something writes the field again.

The second case covers:

- a field whose type or merge changed;
- an enum case that was removed;
- a view `state` whose type changed;
- a new app name, icon or category.

The app name, icon and category are the installed manifest's. The window chrome shows
them from the app's next start.

Adding and removing declarations keeps state. A removed field's values stay in the
record, unread. Renaming keeps state too, because the ids lock keeps each id
(`tilolang ids --rename`). tilolang has no migration rules yet. When it does, this is
where they run.

### How the swap stays inside custody in a session

A dev-mode swap installs a package exactly as any install does: verified by
`tilo-install`, its old version kept while it runs. Then the session's launcher hands
the new module to the process it launched. Nothing else can.

- **Development sessions only.** The launcher follows reinstalls only when two things
  hold:
  - it is built with the developer overrides;
  - it is started with `TILO_LAUNCHER_LIVE_MODULES=1`, which `nested-session.sh` sets.

  A shipped launcher ignores the variable (`package_store::dev_env`). A release
  compositor strips it from every launcher it starts (`launcher::dev_env::
  DEV_OVERRIDE_VARS`).
- **Only module payloads, only pinned launches.** It applies to packages whose
  entrypoint executes their runtime's binary (`[entrypoint] runtime`), launched by the
  shell with a pinned package hash.
- **Verified like a launch.** When the app store's registry changes, the launcher reads
  the new installation of the same package id. It verifies it with the same check a
  launch makes (`run::verified_launch_installation`: the signed record, the payload's
  integrity, the revocation list). It takes the installation only if both hold:
  - the same publisher key signed it;
  - it executes the same runtime binary.

  A build that names another runtime runs only on a new start.
- **Read-only hand-over.** The launcher copies the verified payload into a directory
  it owns, which is bound read-only into the sandbox (`TILO_APP_MODULE_UPDATES`). It
  names the new generation by a rename. The app reads it and checks that it is still
  its own id.

The app's process, its sandbox, its grants and its custody record stay as they were
launched. The only thing that changes is which verified module the runtime interprets.
The code is in `crates/packaging/launcher/src/live_module.rs` and
`apps/tilolang/src/updates.rs`; the generation protocol is `crates/platform/tilo-app-module`.

On the desktop there is no launcher to hand anything over: the tool runs the app itself and
publishes into a directory of its own, private to the person, which the app reads the same
way.

## The runtime loop

### On this desktop

Let `tilolang dev` build the runtime (or give `--runtime BIN` a binary of your own):

```sh
target/fast/tilolang dev apps/tl/tasks --search apps/tl
```

On every change of `apps/tilolang`, `tilolang-tilo`, `tilolang-runtime` or `tilolang` it
runs `cargo build --locked --profile fast -p tilolang-app --bin tilolang-app`, and when the build
succeeds it stops the running app (`SIGTERM`, so it flushes its checkpoint) and starts it
again on the new binary. The resources are in its local store; the scroll, selection and
open item come back from the checkpoint.

### In a session

```sh
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json --build-runtime
```

`tilolang dev` then:

1. builds `tilolang-app` thin with `dist/dev/tilolang-runtime-build.sh` (cargo). It builds again on every
   change of `apps/tilolang`, `tilolang-tilo`, `tilolang-runtime` or `tilolang`;
   a failed build leaves the apps on the last good runtime;
2. bakes the binary into a runtime bundle signed by a key the launcher trusts;
3. packs it as a runtime package (`so.tilo.runtime.tilolang.dev`, providing
   `tilolang/0.1.0+dev`) and installs it with `tilo-install`;
4. builds the apps naming `tilolang/0.1.0+dev`;
5. restarts each of their windows: to the front, `tab.close`, and once the window is
   gone `tab.reopen`, which relaunches it in place as a session restore does.

`--runtime BIN` instead follows a binary you build yourself, and re-bakes on every
new build of it.

Every new build re-bakes and reinstalls the bundle, and the apps restart on it. Save `.tl` files as before: those builds name the same runtime
and are taken in place.

The binary is built thin, as the session's own runtime bundle carries it: a few
megabytes, linked against the session's `libruntime_dylib.so`. A thin build runs only on
the runtime library of its own Cargo graph, because a Rust dylib's symbols carry crate
hashes, so it is built with the default collection's runtime plan, as the
nested session's build does, and the tool refuses a build whose library is not the one
the session runs: start the session on a build of the same checkout.

## Measuring

```sh
scripts/headless-examples/tilolang-dev-loops.py --runs 5              # in a session
scripts/headless-examples/tilolang-dev-loops.py --desktop --runs 5    # on this desktop
```

The script edits, waits for the change on screen, and prints the medians. Both runs are
headless, on lavapipe, under a session's compositor; the desktop run is the tool's own
window, with no session, as a plain client of that compositor. `--busy N` keeps N
CPU-bound processes running, so a run shows the loop under load too, and every sample
prints the load average it saw.

In a session, medians of 5 runs on 2026-10-27, headless on lavapipe, laptop load average
around 12:

| Loop | Median | Runs |
|---|---|---|
| App, save to the app's first frame of the new build | 47 ms | 48, 279, 46, 47, 46 ms |
| App, save to the compositor's next frame of the app | 54 ms | 67, 299, 53, 54, 53 ms |
| Runtime, edit to the restarted app's first frame | 68.4 s | 38.2, 97.9, 89.5, 68.4, 58.6 s |
| Runtime, of which the build | 63.6 s | 35.1, 95.6, 81.9, 63.6, 57.0 s |
| Runtime, after the build: bake, install, restart | 3.1 s | |

On this desktop, saving the script's copy of `apps/tl/tasks` in `tilolang dev OUT/src/tasks
--search OUT/src`, medians of 5 runs on 2026-11-02, in the same session, laptop load
average around 23 (the host is shared):

| Desktop loop | Save to the app's frame | Of which: the tool's build | Of which: read to draw | Save to the presented frame |
|---|---|---|---|---|
| Warm saves, load 23.2 | 31 ms | 14 ms | 11 ms | 43 ms |
| First save after start, load 23.2 | 38 ms | 22 ms | 12 ms | 69 ms |
| Warm saves, 8 CPU-bound processes more, load 25.4 | 36 ms | 24 ms | 12 ms | 69 ms |

The tool reported each as `built in 14 ms, published in 15 ms; the running app takes it
with its state`, and the app logged `draws build 2 at <ms>, 11 ms after it arrived`. The
first save is the app's first take-over, so its module is interpreted and drawn for the
first time; every warm save takes the build in place with its state.

Before, with cargo debug builds of the tool and the runtime, the same loop took 174 ms at
the median from the save to the app's frame and 274 ms to the presented frame (load 34);
a debug tool on a `fast` runtime still took 122 ms (load 21). The tool's build and the
app's read to draw were each several times what they are now, and a local build staged a
payload, packed a `.tapp` and signed it for nothing, then read the platform's packages
again on every build.

In the app loop, compiling and packing take 1 to 2 ms. `tilo-install` takes
140 to 170 ms, but the app takes the build as soon as the install commits its
registry row, before the installer has finished. The one slow run was an install
that waited on the disk.

- **The app loop** is timed from the save to two points:
  - the app's own first frame of the new build: the app logs `draws build N at <ms>`
    when it draws it;
  - the first frame after it that the compositor presented, which the app logs as a
    `tilo-display-frame` row with `TILO_PROFILE_PRESENTATION=1`.

  The second adds the app's rendering of the frame, its hand-over and the compositor's
  presentation: 12 ms at the median here, since a changed placeholder repaints little.
  The session table above is from before these rows: its second row is the compositor's
  own frame counter, which added 7 ms then.
- **The runtime loop** is timed end to end: from the edit of a runtime source, through
  the build, the bake, the install and the restart, to the restarted app's first
  presented frame.

## Running a change in a real session

The desktop loop is the person's own: the tool starts the app from their terminal and the
app's record is its local store. A tilolang app inside a Tilo session is an installed app,
so a changed build there still needs two things a release launcher does not grant:

1. **A runtime key the person authorizes.** A release launcher trusts only the image's
   runtime publishers. Today the session points it at its own list, which is a
   developer override. The desktop needs a person-held list of development runtime keys
   that a release launcher also reads. Settings would offer "Trust runtimes I build",
   recording the key `tilolang dev` signs with. The launcher would take such a key only
   for runtime versions marked `+dev`, and only for apps the person develops.
2. **Live modules per app, by grant.** A release launcher never follows reinstalls. It
   needs a per-app development grant that the person gives in Settings and the shell
   records like other grants. The shell would then pass a launch flag for that app
   only, not an environment variable. The launcher follows reinstalls of a granted
   app's package with the checks it makes now: same publisher, same runtime, verified
   like a launch.
3. **Open and restart from a terminal.** The tool opens and restarts apps through the
   session's test control socket, which only developer builds serve. On a desktop it
   needs the person's action door: `applications <id>`, `tab.close` and `tab.reopen`,
   invoked as the person. The admin socket refuses processes the shell launched, and a
   terminal is one.
