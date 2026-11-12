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

The hooks catch formatting and commit-message mistakes before you push.

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

```sh
cargo build --locked -p example-counter
cargo test --locked -p example-counter
```

For an app, `cargo run --locked -p notes` opens it in its own window (replace
`notes` with the package name of the app you changed). Append a test name to
`cargo test` to run only that test.

Some packages are large (`tilo-session` pulls the whole desktop in); check them
with `cargo check --locked -p tilo-session --tests` while you iterate and run their
tests once before you send the change.

Repository rules (formatting, licences, dependency hygiene, platform code only
in `crates/platform/`) are checked by the policy suite:

```sh
bash tools/policy/run.sh
```

[`docs/policies.md`](docs/policies.md) explains each rule.

## Run it

One command each, from anywhere in the checkout. Each builds only what changed,
takes `--help` for its options, and `--check` to start it, ask it something,
stop it and say whether it worked.

```sh
cargo desktop              # the Tilo desktop in a window on your desktop
cargo headless             # the session and its apps with no window, for agents and tests
cargo web                  # the web desktop, served by the local tilo.sh stack
```

`cargo desktop` (Linux with a Wayland desktop) installs the apps as signed
packages in private state and opens the desktop in a window of its own; pick
apps with `--apps "notes chat"`. It and `cargo headless` have their own runtime
directory, session bus and state, and never reach your session bus, the system
bus, systemd or logind; `cargo desktop --check` proves that on any machine,
without a display. Close the window or press Ctrl-C to stop either.
`cargo headless` prints how to drive it once it answers (`tilo-control` on its
control socket); `--ready FILE` writes the same as JSON for a script.

`cargo web` builds the desktop bundle and serves it with the relay, front door
and web entry production runs, on 127.0.0.1; it loads the page's modules and
runtime back before it prints the address, and fails naming what is missing.
The stack keeps running until `cargo web down`. It needs the
`wasm32-unknown-unknown` Rust target (`rustup target add
wasm32-unknown-unknown`), the `wasm-bindgen` CLI at the version
`crates/browser/tilo-browser-runtime/Cargo.toml` pins, `curl`, `bwrap`,
`openssl` and `python3`; [testing tilo.sh](docs/testing-tilo-sh.md) covers the stack.

A single app in its own window needs none of these: `cargo run --locked -p
notes` (on macOS too). For changes to the OS image, boot or devices, use a
development VM: `cargo run --locked -p tilo-vm -- guide quickstart`.

## Write your first app

Apps use the `tilo-app` SDK and the `tilo-app` command. The quickest start is
an example from `examples/apps`:

```sh
cargo run --locked -p tilo-app-cli -- new my-list --from-example shared-checklist
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
