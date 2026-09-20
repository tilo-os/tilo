# Native installer validation

The Linux installer builds and runs natively on AArch64. Validation used the
locked workspace dependencies and the normal release profile (including LTO):

```sh
cargo test -p installer --locked
cargo build --release -p installer --locked
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json \
  bash os/tests/installer-render-smoke.sh \
  target/release/tilo-installer /path/to/new-render-results
```

Select the Vulkan driver installed for the machine being tested; the example
uses native ARM software rendering. The script accepts an existing installer
binary and a new output directory, records its hash and linked libraries, and
renders sample states with bounded execution times. It invokes only `--shots`,
which uses fixture data rather than performing installation actions.

## AArch64 results

The native test run passed 146 library tests and four binary tests, with no
failures or ignored tests. The release executable is an AArch64 ELF with SHA-256
`52874c02c0b0e7e99e5f9a93ffb178c9f8bbbf2dfc78cf340e74558f0d8b011d`.
All dynamic dependencies resolved in the Fedora 42 WSL build environment.

The initial diagnostic used Fedora Rust 1.94.1. The environment was corrected
to use the repository-pinned Rust 1.97.1 through rustup, and all 150 tests,
the release build, and all 19 screen renders passed again. The hash above is
the pinned-toolchain artifact. Its inspected 800×600 review screen is
byte-identical to the earlier rendering (SHA-256
`ea0dcbdba8e1b60ac727fcab383594421ca400446be91854ad659113465c51d6`).

All 16 screen states rendered at 1280×800, plus disk, account, and review at
800×600. This includes alongside-install, recovery, and failure states. The
800×600 review screenshot was inspected: the summary text is readable and the
page has a vertical scrollbar for content below the viewport.

These checks establish native execution and software rendering. They do not
establish input/scroll interaction, physical GPU operation, installation onto
a virtual or physical disk, or operation inside the final signed installer
image. Those remain separate acceptance requirements.
