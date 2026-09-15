# Native WPE browser validation

The `wpe-builder-1` target in `os/Containerfile` builds libwpe 1.16.3,
WPEBackend-fdo 1.16.1, and WPE WebKit 2.52.6 from verified cached sources.
The ARM build completed all 9,423 WebKit compilation/link steps with two jobs
and no network access during the engine build.

The local ARM engine image is `localhost/qos-wpe:arm64-dev`, image ID
`22e6e2e9b40a6676b36cb55a9e6b85b15b1205a368b34af6b634f54da345015b`.
This local image ID is build evidence, not a published or signed release pin.
The staged MiniBrowser, WPEWebProcess, WPENetworkProcess, and WPEGPUProcess are
AArch64 ELF executables. Their dynamic dependencies resolved in the native
build environment.

## Installed runtime smoke test

Overlay `/wpe/` from the engine image into `/` of a test buildroot, matching the
OS Containerfile's installation layout, and run `ldconfig`. MiniBrowser must
report `WPE WebKit 2.52.6` with `--version`.

Run `python3 os/tests/wpe-webdriver-smoke.py` inside an isolated container with:

- an init process (`podman run --init`), which manages browser child processes;
- an ordinary UID and all capabilities dropped;
- no external network (`--network=none`); WebDriver uses container loopback;
- writable `HOME` and `XDG_RUNTIME_DIR` and a private `/tmp`;
- `LIBGL_ALWAYS_SOFTWARE=1` when testing without a GPU.

The acceptance run passed with Podman's default seccomp filter and WebKit's own
sandbox enabled. An earlier diagnostic run also passed with the outer filter
disabled; the final test does not require that override.

The smoke test starts the shipped WebDriver and headless MiniBrowser, loads a
local data URL, verifies document title, JavaScript arithmetic and DOM access,
computed CSS, and exact canvas pixel values. It also requires an 800 by 600 PNG
screenshot. All checks passed on the native ARM build. The run reported a
missing accessibility bus; accessibility integration is not covered here.

This establishes software-rendered engine operation in its test environment.
It does not establish physical GPU acceleration, the qOS browser application
integration, the final frozen RPM runtime, or signed release acceptance.
