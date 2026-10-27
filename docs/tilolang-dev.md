# Iterating on a tilolang app

Two loops, both against a real Tilo session: the app runs as an installed app, launched
by the launcher, in its sandbox, under its own id and custody.

- **The app loop.** Save a `.tl` file. The running app shows the change in about
  50 ms, with its state: its resources, its window and lens state, the scroll,
  the selection and the open item.
- **The runtime loop.** Change the runtime (`apps/tilolang`, `tilolang-tilo`,
  `tilolang-runtime`) and build it. The running apps restart on the new runtime in
  about 70 s, almost all of it the build on t. The session keeps running.

## Start

You need a nested-session build (`dist/dev/nested-session-build.sh
target/nested-session`, which `t outputs nested-session` makes) and the tool, built
with the nested session's profile:

```sh
cargo build --profile nested --config 'profile.nested.inherits="release"' \
    --config profile.nested.lto=false --config profile.nested.codegen-units=16 \
    --config profile.nested.incremental=true -p tilolang-cli --bin tilolang
```

`cargo` builds on the build server (t) and copies the binary back; the examples
below call the result `tilolang` (`target/nested/tilolang`).

```sh
# Start a session and the app in it; from then on, every save reaches the app.
tilolang run apps/tl/notes --search apps/tl --headless --software
```

To work in a session that is already running, attach to it:

```sh
dist/dev/nested-session.sh --headless --software --state /tmp/s --ready /tmp/s.json &
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json
```

`--session` takes the `--ready` file, the session's runtime directory or its control
socket.

## The app loop

On every save `tilolang dev`:

1. compiles the package in-process (tens of milliseconds);
2. on an error, prints the diagnostics and stops: the app keeps running the last good
   build;
3. signs the build into a `.tapp` and installs it with `tilo-install`, like any
   install;
4. says in one line what the running app does with it.

The running app takes the new build in one of two ways:

- **In place.** This is the usual case. Resources live in the room record and view
  state lives in the runtime, both keyed by declaration id. Scroll, selection and the
  open item live in the window. So the new module takes them over: the views are opened
  again on the same windows, under the same shell.
- **Starting over.** If the new build cannot read what the old one wrote, the tool
  says why, for example `type Note: the field \`done\` changed from Bool to Int`. The
  app then starts over on the new build and the room record, as its next start would:
  every resource is still there. View state and the open item start fresh. Nothing is
  deleted. A value of the old type stays in the record until something writes the field
  again.

The second case covers:

- a field whose type or merge changed;
- an enum case that was removed;
- a `window`/`lens` state whose type changed;
- a new app name, icon or category.

The app name, icon and category are the installed manifest's. The window chrome shows
them from the app's next start.

Adding and removing declarations keeps state. A removed field's values stay in the
record, unread. Renaming keeps state too, because the ids lock keeps each id
(`tilolang ids --rename`). tilolang has no migration rules yet. When it does, this is
where they run.

### How the swap stays inside custody

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
`apps/tilolang/src/updates.rs`.

## The runtime loop

Let `tilolang dev` build the runtime from the checkout:

```sh
tilolang dev apps/tl/notes --search apps/tl --session /tmp/s.json --build-runtime
```

`tilolang dev` then:

1. builds `tilolang-app` with the nested profile through `cargo` (on the build
   server), and again on every change of `apps/tilolang`, `tilolang-tilo`,
   `tilolang-runtime` or `tilolang`; a failed build leaves the apps on the last
   good runtime;
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

The binary is a full build of `tilolang-app`, without the `thin` feature, about
110 MB optimised (an unoptimised one is 355 MB). A thin build
must come from the same Cargo graph as the session's `libruntime_dylib.so`, because a
Rust dylib's symbols carry crate hashes. A full build links everything it needs and
carries no such tie. The lazily opened engines (highlighting, ad blocking) are features
the full build does not turn on.

### Local cargo or the build server

Builds go through t, the repository's build server, as every build does: `cargo`
forwards there. Measured on 2026-10-27, rebuilding `tilolang-app` with the nested
profile after a one-line edit of `apps/tilolang/src/updates.rs`:

| Builder | Wall time per rebuild |
|---|---|
| t (`cargo`, the default) | 81 s, 126 s, 650 s: queueing behind other jobs, then a 43 s to 5.7 min build, then copying the 108 MB binary back |
| this machine (`TILO_CARGO_VIA_T=1 cargo`) | 10.7 s, 10.6 s, 10.9 s once warm; 2.5 min after an environment change invalidated the build scripts |

A build on this machine is several times faster, but it is not used. Local builds
compete with everything else on the laptop for memory, and they contributed to an
out-of-memory kill of the desktop's terminals. t's build time is what keeps the
runtime loop above a minute, and is tracked as t's to fix.

## Measuring

```sh
scripts/headless-examples/tilolang-dev-loops.py --runs 5
```

The script edits, waits for the change on screen, and prints the medians. The
runtime loop's builds go through t like any build. Medians of 5 runs on 2026-10-27,
headless on lavapipe, laptop load average around 12:

| Loop | Median | Runs |
|---|---|---|
| App, save to the app's first frame of the new build | 47 ms | 48, 279, 46, 47, 46 ms |
| App, save to the compositor's next frame of the app | 54 ms | 67, 299, 53, 54, 53 ms |
| Runtime, edit to the restarted app's first frame | 68.4 s | 38.2, 97.9, 89.5, 68.4, 58.6 s |
| Runtime, of which the build on t | 63.6 s | 35.1, 95.6, 81.9, 63.6, 57.0 s |
| Runtime, after the build: bake, install, restart | 3.1 s | |

In the app loop, compiling and packing take 1 to 2 ms. `tilo-install` takes
140 to 170 ms, but the app takes the build as soon as the install commits its
registry row, before the installer has finished. The one slow run was an install
that waited on the disk.

- **The app loop** is timed from the save to two points:
  - the app's own first frame of the new build: the app logs `draws build N at <ms>`
    when it draws it;
  - the first new frame of the app that the nested compositor received after that.

  The second adds the app's rendering of the frame and its hand-over to the
  compositor: 7 ms at the median here, since a changed placeholder repaints little.
- **The runtime loop** is timed end to end: from the edit of a runtime source, through
  the build, the bake, the install and the restart, to the restarted app's first
  presented frame.

## On an installed desktop

The end state is the same loop on the person's own installed Tilo desktop, with no
nested session: a tilolang app running on a runtime they just built. `tilolang dev`
already uses the path that carries over:

- **Runtime selection.** The launcher picks the runtime bundle by the version the app
  names (`[entrypoint] runtime = "tilolang/<version>"`). It looks in the person's own
  store first (`$XDG_DATA_HOME/tilo-runtime`). A development runtime is a runtime
  package with a version of its own (`tilolang/0.1.0+dev`) and a package id of its own,
  installed with `tilo-install` like any runtime. Only the apps that name it run on it.
- **Installs.** App builds are signed `.tapp`s installed with `tilo-install`.

What the launcher and the desktop still need:

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
