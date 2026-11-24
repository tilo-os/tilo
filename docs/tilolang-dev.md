# Iterating on a tilolang app

`tilolang dev` runs the package inside a Tilo session, as this device's development build of
the app. Save a `.tl` file and the running app shows the change in about 50 ms, with its
state: its resources, its view state, the scroll, the selection and the open item. A compile
error is printed and the app keeps running the last good build.

The build runs under its own id and ownership (`<app>.dev.<device key>`), installed and
signed by the session's daemon with the device's own key: launched by the launcher, in its
sandbox, apart from any installed app and with none of that app's grants, until the person
gives this build some. The tool never reads the device's key and signs nothing.

The runtime is the one the session baked when it started: `tilolang-process`,
`tilolang-tilo`, `tilolang-runtime` and `tilolang` from the checkout the session was built
from. To change it, restart the session on a new `dist/dev/nested-session-build.sh` build.

## Getting started

This needs a Tilo checkout: run the commands below in one, or name it with `--repo DIR` or
`$TILO_REPO`. Outside one, `tilolang run` and `tilolang dev` say so and stop.

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
tilolang dev apps/tl/notes --search apps/tl
```

`tilolang dev` stages the build, starts the session, and asks the session's daemon to install
and start it (`packages#install-dev`, then `--run-app <id>`): the daemon marks, packs and
signs the build with the device's key, and the launcher runs it. If the run fails at any point
after the session started, the tool stops the session: the shell, its apps and its services.

To work in a session that is already running, attach to it:

```sh
dist/dev/nested-session.sh --headless --software --state /tmp/s --ready /tmp/s.json &
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json
```

`--session` takes the `--ready` file, the session's runtime directory, its control
socket or its state directory. Without it, `--headless` (no window), `--software` (lavapipe
only) and `--state DIR` (the session's identity, apps and workspace) control the session the
tool starts.

## What reaches the app

What the loop counts as a save: anything that changes what a build reads. That is every
`.tl` file of the package and of the packages it uses, the translations in their
`locales/`, and entries added, removed or renamed anywhere below their directories, a
whole package directory moved in included (the loop watches the directory above each
package, so a package replaced whole is taken again). Then it compiles once: the events of
one save, and of one build writing a file in steps, are gathered for a few tens of
milliseconds first.

If the sources use a type this build of the tool does not know (`E013 unknown type`) and
the checkout's own sources are newer than the tool, the loop says so instead of leaving the
error to look like a mistake in the package: rebuild the tool and start `tilolang dev`
again.

The profile is not a detail of this loop: it compiles each saved package, interprets it and
draws every frame of its window, and an unoptimised build of it does each of those several
times slower. `nested` is the workspace's answer for a local build that must run at
near-release speed, and it is the profile the session's own build uses.

The session follows reinstalls of the apps its tilolang hosts run: a saved build is installed
over the last one under the same id, verified as a launch would, and handed to the host
running the app, whose instances take it over in place or start over with their data.
`Ctrl-C` stops the tool and the session it started; closing the window stops the loop.

## In a real session

This needs a Tilo checkout: run the commands below in one, or name it with `--repo DIR` or
`$TILO_REPO`. Outside one, `tilolang run` and `tilolang dev --session` say so and stop.

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

`tilolang run` stages the build, starts the session, and asks the session's daemon to
install and start it (`packages#install-dev`, then `--run-app <id>`): the daemon marks,
packs and signs the build with the device's key, and the launcher runs it. If the run fails
at any point after the session started, the tool stops the session: the shell, its apps
and its services.

To work in a session that is already running, attach to it:

```sh
dist/dev/nested-session.sh --headless --software --state /tmp/s --ready /tmp/s.json &
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json
```

`--session` takes the `--ready` file, the session's runtime directory, its control
socket or its state directory. Without it, `--headless` (no window), `--software` (lavapipe
only) and `--state DIR` (the session's identity, apps and workspace) control the session the
tool starts.

To type tilolang at the app's data, as the session's person, attach the REPL the same way:

```sh
tilolang repl --session            # the only session running, or the one you pick
tilolang repl --session /tmp/s     # that one
```

The REPL reaches the session through Tilo's API, which every session serves its own person's
programs on this computer ([the Tilo API](developers/api.md)). What it changes shows in the
session's History as the person's, and `undo` takes it back.

There, on every save `tilolang dev`:

1. compiles the package in-process (tens of milliseconds);
2. on an error, prints the diagnostics and stops: the app keeps running the last good
   build;
3. stages the build as the app's manifest and payload, and the session's daemon installs
   it as this device's development build (`packages#install-dev`): the daemon marks it,
   packs it and signs it with the device's key, under the id its marking derives
   (`<app>.dev.<device key>`). The tool reads no publisher key and signs nothing;
4. says in one line what the running app does with it.

## The gallery

`apps/tl/gallery` places every component of the toolkit with the values an app gives
it. Open it in a session:

```sh
tilolang dev apps/tl/gallery --search apps/tl                   # a nested session, the gallery in it
tilolang dev apps/tl/gallery --search apps/tl --session /tmp/s.json   # into a nested session already running
```

## The app loop

A new build is taken one of two ways:

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

### How the swap stays inside ownership in a session

A dev-mode swap installs a package as the device's own development build
(`packages#install-dev`): the session's daemon marks the staged build, packs and signs it
with the device's key, and installs it verified, its old version kept while it runs. Then
the session hands the new module to the tilolang process that runs the app's instances.
Nothing else can.

- **Development sessions only.** The session follows reinstalls only when two things
  hold:
  - it is built with the developer overrides;
  - it is started with `TILO_LAUNCHER_LIVE_MODULES=1`, which `nested-session.sh` sets.

  A shipped session ignores the variable (`package_store::dev_env`).
- **Verified like a launch.** When the app store's registry changes, the session reads
  the new installation of each app a running host runs. It verifies it with the same
  check a launch makes (the signed record, the payload's integrity, the revocation
  list). It hands it over only if the host would run it: the same author signed it
  and it names the same runtimes. A build that names another runtime runs only on a
  new start.
- **Over the host's action API.** The module goes to the host in a sealed memfd, on the
  action API the session hands it instances on. The host checks that it is still the app's
  own id, and every instance of the app takes it over.

The windows, their sandboxes and grants stay as they were launched. The only thing
that changes is which verified module the host interprets. The code is in
`crates/core/tilo-runtime/src/device/processes.rs` and `crates/lang/tilolang-process`.

## The runtime

A session runs the tilolang runtime it baked when it started, from the checkout's own
`tilolang-process`, signed by the session for its own launcher. To change the runtime, restart
the session on a new `dist/dev/nested-session-build.sh` build: its script bakes the runtime
bundle again. Saved `.tl` files reach the app the same way, and those builds name the same
runtime and are taken in place.

The binary the session bakes is built thin, as its runtime bundle carries it: a few
megabytes, linked against the session's `libruntime_dylib.so`. A thin build runs only on
the runtime library of its own Cargo graph, because a Rust dylib's symbols carry crate
hashes, so it is built with the default collection's runtime plan, as the
nested session's build does, and the tool refuses a build whose library is not the one
the session runs: start the session on a build of the same checkout.

## Measuring

```sh
scripts/headless-examples/tilolang-dev-loops.py --runs 5
```

The script edits, waits for the change on screen, and prints the medians. It runs headless,
on lavapipe, in a session. `--busy N` keeps N CPU-bound processes running, so a run shows the
loop under load too, and every sample prints the load average it saw.

The script reports each save as `built in <ms>, published in <ms>; the running app takes
it with its state`, and the app logs when it drew the new build. The first save after a
start is the app's first take-over, so its module is interpreted and drawn for the first
time; every warm save takes the build in place with its state.

In the app loop, compiling and packing take 1 to 2 ms. The install is the session
daemon's, and the app takes the build as soon as the install commits its registry row,
before the installer has finished. A slow run is normally an install that waited on the
disk.

- **The app loop** is timed from the save to two points:
  - the app's own first frame of the new build: the app logs `draws build N at <ms>`
    when it draws it;
  - the first frame after it that the compositor presented, which the app logs as a
    `tilo-display-frame` row with `TILO_PROFILE_PRESENTATION=1`.

  The second adds the app's rendering of the frame, its hand-over and the compositor's
  presentation: 12 ms at the median here, since a changed placeholder repaints little.
## Running a change in a real session

A tilolang app inside a Tilo session is an installed app, so a changed build there still
needs things a release launcher does not grant:

1. **Live modules per app, by grant.** A release session never follows reinstalls. It
   needs a per-app development grant that the person gives in Settings and the session
   records like other grants, in place of an environment variable. The session follows
   reinstalls of a granted app's package with the checks it makes now: same author,
   same runtimes, verified like a launch.
3. **Open and restart from a terminal.** The tool asks the session's daemon to start the
   app (`--run-app <id>`), the launch a person's own command takes. Restarting a window in
   place would still need the person's action API (`tab.close` and `tab.reopen`), invoked
   as the person: the admin socket refuses processes the shell launched, and a terminal is
   one.
