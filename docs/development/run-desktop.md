# Run the desktop in a window

Use a windowed desktop for shell layout, navigation and app interaction. It does
not replace your login session. Tilo runs as a native application on macOS; only
Linux provides the Wayland login desktop and Tilo OS.

## Linux: desktop with packaged apps

This route needs hosted [t](../t.md) access and a Wayland desktop. From the
checkout root:

```sh
dist/dev/nested-session.sh --apps "notes chat"
```

This downloads a matched build from [t](../t.md), installs signed development
apps into private state, and starts the shell. No root or host installation is
needed. Close the window or press Ctrl-C to stop it.

Use the [nested-session tool guide](tools/nested-session.md) for prerequisites,
state, logs, headless operation and collaboration between two sessions. The app
selection controls installation, not the remote build's app set.

## macOS: native desktop application

Build the shell target with the pinned toolchain and native dependencies from
[setup](setup.md), using your configured build executor:

```sh
cargo build --locked -p shell --bin shell
./target/debug/shell
```

This runs the desktop's workspace tree, chrome and in-process apps in a native
macOS window, not a Linux compositor or a VM. Workspace and record storage
defaults to the local data directory's `tilo/macos-shell`; Files initially opens
your home directory. To choose those locations:

```sh
./target/debug/shell --state-dir "$HOME/tilo-shell-dev" --files-dir "$HOME/src"
```

A profile can be open in only one shell process. Invalid saved state is
reported, not silently replaced. `--state-dir` selects workspace/app records; it
is not a disposable HOME or a sandbox for host file access.

The macOS runner's `--help` lists its supported options.
`--shots /path/to/shell.png` renders one PNG and exits; it expects a filename,
not a directory. Linux flags such as `--backend drm` do not apply.

## Debug responsiveness

Dev and test builds keep application and toolkit code unoptimised. The compositor
uses `opt-level = 1` for UI traversal and serialization monomorphised inside it,
with full debugger information, debug assertions and overflow checks. Renderer,
geometry and GPU command-processing dependencies are optimised separately; GPU
validation stays enabled. Release profiles are unchanged.

Some compositor locals can be optimised out. For exact stepping, build with
`cargo build --config 'profile.dev.package.shell.opt-level=0' --locked -p shell --bin shell`.
Measure event processing as well as rendering: fast GPU submission cannot
compensate for unnecessary work before input. The
[performance scripts](../../scripts/perf/README.md#windowed-desktop-input) compare
private sessions without driving the user's desktop.

Lock verification and refusal feedback have a separately bounded status row:
the answer cross-fades in place, a refusal gives one brief nudge, and reduced
motion suppresses that movement. The field and row keep their geometry, and no
animation ticks after the answer rests. Lock edges, display changes and
unproven scene differences remain full-frame boundaries; retained rendering
never relies on a swapchain image retaining pixels.

## Need a different test surface?

- [Development VM](dev-vms.md): the Linux OS, services, DRM and virtual input.
- [Installed Linux session](host-session.md): host login, locking and real
  outputs.
- [Devices and boards](devices.md): firmware, physical drivers and boot
  changes.
- [Headless testing](../headless-testing.md): scripted compositor checks and
  images.

A windowed run does not establish screen-lock, suspend, KMS or physical hardware
correctness.
