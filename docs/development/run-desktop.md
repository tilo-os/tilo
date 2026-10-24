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

## Need a different test surface?

- [Development VM](dev-vms.md): the Linux OS, services, DRM and virtual input.
- [Installed Linux session](host-session.md): host login, locking and real
  outputs.
- [Boards and kernels](hardware.md): firmware, physical drivers and boot
  changes.
- [Headless testing](../headless-testing.md): scripted compositor checks and
  images.

A windowed run does not establish screen-lock, suspend, KMS or physical hardware
correctness.
