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

Some packages are large (the compositor, `shell`, is the largest); check them
with `cargo check --locked -p shell --tests` while you iterate and run their
tests once before you send the change.

Repository rules (formatting, licences, dependency hygiene, platform code only
in `crates/platform/`) are checked by the policy suite:

```sh
bash tools/policy/run.sh
```

[`docs/policies.md`](docs/policies.md) explains each rule.

## Run the desktop

On a Linux machine with a Wayland desktop, build and start the Tilo desktop in
a window, with the apps you name installed as signed packages in private
state:

```sh
dist/dev/nested-session.sh --apps "notes chat"
```

It does not touch your login session; close the window or press Ctrl-C to stop
it. `--headless` runs it offscreen and `--help` lists the other options. On
macOS the shell runs as a native app: `cargo run --locked -p shell --bin shell`.

For changes to the OS image, boot or devices, use a development VM:
`cargo run --locked -p tilo-vm -- guide quickstart`.

## Run the web desktop

The same apps run in the browser, compiled to WebAssembly. You need the
`wasm32-unknown-unknown` Rust target, the `wasm-bindgen` CLI at the version
`crates/browser/tilo-browser-runtime/Cargo.toml` pins, and Node.js:

```sh
rustup target add wasm32-unknown-unknown
crates/browser/tilo-browser-runtime/scripts/build.sh "$PWD/target/web-desktop" \
  --bundle desktop --profile quick
python3 -m http.server -d target/web-desktop
```

Then open <http://localhost:8000> in a browser with WebGPU.

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

`main` is the integration branch and moves fast; releases are cut from
`stable`.

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
