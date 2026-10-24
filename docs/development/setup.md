# Build prerequisites

For the checkout → change → test → PR workflow, use the
[contribution guide](../../CONTRIBUTING.md). This page is a reference for
setting up your build environment.

## Rust

Install Rust with [rustup](https://rustup.rs). The checkout's
`rust-toolchain.toml` selects the compiler and components:

```sh
rustup show
```

Build from the repository root and select your package with `-p`; a
workspace-wide build includes OS services your host may not support.

## Apps and native libraries

SDK/app builds need Python 3.11 or newer as `python3`. On macOS,
`brew install python` supplies a newer version than the system Python.

Install the native libraries needed by your component:
[Linux](platforms/linux.md), [macOS](platforms/macos.md), or
[Windows](platforms/windows.md). Rust-only libraries often need none. Android
app-host work has its own [setup](platforms/android.md).

On Linux, `tools/ci/check-build-env.sh` checks the full desktop build
environment. It is useful for shell work, but don't install everything it lists
just to work on one app or a library.

## Optional hosted builds

[t](../t.md) can build and test on the project's builders instead of your
machine. Hosted access currently needs a maintainer-issued token; ask us if you
need it. You don't need a build-service account to contribute.
