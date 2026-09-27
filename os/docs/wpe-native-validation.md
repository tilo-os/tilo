# Native WPE browser validation

The `wpe-builder-1` target in `os/Containerfile.wpe` builds libwpe 1.16.3,
WPEBackend-fdo 1.16.1, and WPE WebKit 2.52.6 from verified cached sources.
The ARM build completed all 9,423 WebKit compilation/link steps with two jobs
and no network access during the engine build.

The local ARM engine image is `localhost/tilo-wpe:arm64-dev`, image ID
`22e6e2e9b40a6676b36cb55a9e6b85b15b1205a368b34af6b634f54da345015b`.
This local image ID is build evidence, not a published or signed release pin.
The staged MiniBrowser, WPEWebProcess, WPENetworkProcess, and WPEGPUProcess are
AArch64 ELF executables. Their dynamic dependencies resolved in the native
build environment.

## Existing local buildroot identity

The retained native buildroot resolves as
`localhost/tilo-wpe-buildroot@sha256:1c17950cbd3d6a31c8fab1a9f3880abc91f7b656ba7a0ed54eaafc452b9bfb59`.
Its image ID is
`5c42be1f7edfd10e1c6f26144cf812ed585f2576ae864a69cba01c72d4b2059a`.
A read-only, network-disabled verification container confirmed all 1,126
installed package identities match `wpe-buildroot-packages.txt`, its recorded
and native architectures are aarch64, and no live repository files remain.
Compiler/build tools and the GBM, Wayland, epoxy, input and systemd pkg-config
probes passed. The engine image above inherits all 66 buildroot layers exactly.

Local preparation selects this immutable reference through `WPE_BUILDROOT_REF`;
it does not need to resolve or rebuild those dependencies again. Evidence is
retained under `wpe-buildroot-local-verification/` in the preparation directory.
The first probe used the wrong pkg-config name `libepoxy`; the corrected
`epoxy` probe passed. This establishes local buildroot identity and continuity
with the tested engine, not signature verification, production promotion or
browser operation against the final OS runtime package set.

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
It does not establish physical GPU acceleration, the Tilo browser application
integration, the final frozen RPM runtime, or signed release acceptance.
