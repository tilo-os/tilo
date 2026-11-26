# Contributing to Tilo

Tilo is early alpha software. Code, bug reports, accessibility testing,
translations, design feedback and documentation are all welcome, from people
and from their coding agents.

[Forgejo](https://code.tilo.so/tilo/tilo) is the main repository; the
[GitHub repository](https://github.com/tilo-os/tilo) is a mirror. Please open
issues and pull requests on Forgejo.

## Report a bug or propose a change

[Open an issue](https://code.tilo.so/tilo/tilo/issues/new/choose). Say what you
did, what happened and what you expected. Reproduction steps, your commit or
image version and logs help; remove private information from logs first. Report
security problems privately as described in [SECURITY.md](SECURITY.md).

Small fixes do not need an issue first. For a large feature or refactor, ask
first so we can point you to the right place and avoid overlapping work.

## Get a checkout

Fork the repository on Forgejo, then:

```sh
git clone https://code.tilo.so/YOUR_USERNAME/tilo.git
cd tilo
git remote add upstream https://code.tilo.so/tilo/tilo.git
git switch -c my-change
python3 tools/dev/install-git-hooks.py
```

The hooks catch formatting, commit-message and build mistakes before your
change leaves the machine. They are Git's own hooks, enabled per clone by that
script; a push they refuse is fixed, never forced with `git push --no-verify`.

## Install the toolchain

- **Rust** through [rustup](https://rustup.rs). The checkout's
  `rust-toolchain.toml` selects the exact toolchain; rustup installs it on the
  first build.
- **Python 3.11 or newer** as `python3`, for the repository tools and for
  staging the SDK's fonts during a build.
- **Native libraries** for the crates you build. Portable crates (the SDK, most
  apps) need only a C compiler and `pkg-config`; the compositor and media code
  need more. On Fedora:

  ```sh
  sudo dnf install gcc pkgconf-pkg-config clang-libs libxkbcommon-devel \
    libseat-devel libinput-devel systemd-devel libdrm-devel mesa-libgbm-devel \
    mesa-libGL-devel mesa-libEGL-devel wayland-devel pipewire-devel \
    fuse3-devel dbus-devel gstreamer1-devel gstreamer1-plugins-base-devel
  ```

  On Debian or Ubuntu the same libraries are `libxkbcommon-dev`, `libseat-dev`,
  `libinput-dev`, `libudev-dev`, `libdrm-dev`, `libgbm-dev`, `libgl-dev`,
  `libegl-dev`, `libwayland-dev`, `libpipewire-0.3-dev`, `libfuse3-dev`,
  `libdbus-1-dev`, `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`
  and `libclang-dev`. [`dist/ci/install-build-deps.sh`](dist/ci/install-build-deps.sh)
  is the complete list CI installs on Fedora, Debian and Arch, and
  `tools/ci/check-build-env.sh` reports what is missing for the whole desktop
  and its tests.

You never need all of Tilo to work on one part of it: build only the packages
you change.

## Build and test one crate

Tilo is one Cargo workspace. Select a package with `-p` and its `name` from its
`Cargo.toml`; the [code map](docs/codemap.md) says which crate owns what.

Builds and tests happen on a build server, through `t`: it compiles the same
cargo commands on warm builds, so the machine you edit on keeps its cores and
its memory for the editor.

```sh
t cargo build --locked -p example-counter
t cargo test --locked -p example-counter
```

For an app, `t cargo build --locked -p messages` builds it on the server, and
`cargo desktop --apps "messages"` opens the desktop with it here (replace `messages`
with the package name of the app you changed). Append a test name to `t cargo
test` to select only that test.

Some packages are large (`tilo-session` pulls the whole desktop in); check them
with `t cargo check --locked -p tilo-session --tests` while you iterate and test
them once before you send the change. `t test` covers the checks your change
reaches, whichever they are.

Repository rules (formatting, licences, dependency hygiene, platform code only
in `crates/platform/`) are checked by the policy suite, which `t` splits into one
check per step:

```sh
t test policy            # the whole suite
t test policy/<step>     # one step; tools/policy/steps.toml names them
```

Dozens of checks reading the whole tree at once take the machine they are on, so
[`tools/policy/run.sh`](tools/policy/run.sh) refuses on a development machine and
says to hand the suite to `t`; its `--local` flag takes it here anyway, for a
machine with no server to hand it to. [`docs/policies.md`](docs/policies.md)
explains each rule.

## Run it

One command each, from anywhere in the checkout. Each builds only what changed,
takes `--help` for its options, and `--check` to start it, ask it something,
stop it and say whether it worked.

```sh
cargo desktop              # the Tilo desktop in a window on your desktop
cargo headless             # the session and its apps with no window, for agents and tests
cargo daemon               # a device daemon in the background, over a profile of its own
cargo web                  # the web desktop, served by the local tilo.sh stack
```

`cargo desktop` (Linux with a Wayland desktop) installs the apps as signed
packages in private state and opens the desktop in a window of its own; pick
apps with `--apps "notes chat"`. It and `cargo headless` have their own runtime
directory, session bus and state, and never reach your session bus, the system
bus, systemd or logind; `cargo desktop --check` proves that on any machine,
without a display. Close the window or press Ctrl-C to stop either.
`cargo headless` prints how to drive it once it answers (`tilo-control` on its
control socket, and an MCP agent with the session's agent key, which the MCP server
admits without a click); `--ready FILE` writes the same as JSON for a script.

`cargo daemon` starts a device daemon for the checkout — the action model, this
device and your workspaces, with no desktop or shell — beside the daemon your
computer runs. Its profile, identity and workspace folders live under its state
directory (`--state`, by default `$XDG_STATE_HOME/tilo-daemon-dev`), and it prints
the two exports that point `tilo`, the SDKs and agents at it. A fresh state founds
a throwaway person of its own, as `cargo headless` does; `--ticket` logs it in as
a device of an existing person with a ticket `tilo device add` printed instead.
`cargo daemon --stop` ends it.

`cargo web` builds the web entry's bundle and serves it with the server, API server
and web entry production runs, on 127.0.0.1; it loads the page's modules and
runtime back before it prints the address, and fails naming what is missing.
The stack keeps running until `cargo web down`. It needs the
`wasm32-unknown-unknown` Rust target (`rustup target add
wasm32-unknown-unknown`), the `wasm-bindgen` CLI at the version
`crates/core/tilo-browser-runtime/Cargo.toml` pins, `curl`, `bwrap`,
`openssl` and `python3`; [testing tilo.sh](docs/testing-tilo-sh.md) covers the stack.

A single app in its own window needs none of these: `cargo run --locked -p
notes` (on macOS too). For changes to the OS image, boot or devices, use a
development VM: `cargo run --locked -p tilo-vm -- guide quickstart`.

## Write your first app

Apps use the `tilo-app` SDK and the `tilolang` command. The quickest start is
an example from `examples/apps`:

```sh
cargo run --locked -p tilolang-cli -- new my-list --from-example shared-checklist
```

[Build an app](docs/app-development.md) walks through the model (resources,
actions and bound views), testing, running on other platforms and publishing.

## Send your change

Commit with a subject of the form `type(scope): description`, for example
`fix(notes): keep the caret after undo`. The types are `build`, `chore`, `ci`,
`docs`, `dev`, `env`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style` and
`test`.

```sh
git add PATHS_YOU_CHANGED
git commit -m "fix(notes): keep the caret after undo"
git push -u origin my-change
```

The push is linted first. The hook works out which packages the pushed commits
touch (Rust and Cargo files, against the ref being updated), checks the changed
files' formatting with rustfmt, and runs Clippy over every target of the host for
those packages, with the features the workspace's whole build gives them. Only
the touched packages are linted; a push without Rust files passes immediately.
Every failure prints the command that reruns it. Packages that build directly on
one of the touched packages through a path dependency are linted too, and a
package the page family names is linted for wasm32; what a change reaches beyond
those, and the packages as they ship, is t's clippy families' work on `main`.

Let a push run to the end. Its lint can take minutes, so run the push as a task
and wait for it: do not stop the task, end an agent's turn, or interrupt the
shell while its push runs. A shell torn down under a push takes its process
group with it, the hook included, and Git reports the hook as killed by a
signal. Never kill processes by pattern on a shared machine; kill only the PIDs
you started.

With [t](https://code.tilo.so/tilo/t) installed, the lint is t's own checks
(`clippy/<package>` and the like), which t answers from its result cache when
nothing that package's build reads has changed: a push retried after a rebase
onto unrelated commits passes in seconds, and a package the new commits of
`main` reach, through a dependency too, is linted again. When `main` moved
while the lint ran, the push is refused: rebase and push again.

A lint that passed stands for the same change after such a rebase. The hook
remembers a pass by the push's own patch and the trees of the linted packages'
own directories, so a retry whose rebase resolved no conflict, onto commits
that touch none of those directories, is sent without linting again, however
wide the push. What only the combination with `main`'s new commits breaks is
found by t's clippy families on `main` and fixed by the next commit.

Without t, or with `CARGO` naming a cargo drop-in (which may build on another
machine), the lint compiles with that cargo, or the `cargo` on your `PATH`.

Then [open a pull request](https://code.tilo.so/tilo/tilo/pulls) from your
fork's branch into `tilo/tilo`'s `main`. Say what changed, why, and what you
tested; a screenshot helps for visual changes. If something would not build or
you could not test it, say so. CI runs on the pull request, and a maintainer
reviews and merges it.

`main` is the integration branch and moves fast; releases are cut by a release
tag (`vMAJOR.MINOR.PATCH`) on a `main` commit, and the tag's run builds, signs
and publishes what the release carries.

If you use an agent, you remain responsible for understanding the change,
having the right to contribute it and reporting accurately what was tested.
[AGENTS.md](AGENTS.md) is written for agents.

## Learn more

- [Architecture](docs/architecture/README.md): the model and where the code is.
- [Workspaces](docs/architecture/workspace.md): the collaboration model in full.
- [Build an app](docs/app-development.md): the app SDK.
- [Security model](docs/security/README.md).

## Licensing

See [LICENSING.md](LICENSING.md). Only contribute code and assets you have the
right to share, and keep third-party licence notices.
