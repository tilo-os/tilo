# Native installer validation

The Linux installer builds and runs natively on AArch64. Validation used the
locked workspace dependencies and the normal release profile (including LTO):

```sh
cargo test -p installer --locked
cargo build --release -p installer --locked
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json \
  bash os/tests/installer-render-smoke.sh \
  target/release/qos-installer /path/to/new-render-results
```

Select the Vulkan driver installed for the machine being tested; the example
uses native ARM software rendering. The script accepts an existing installer
binary and a new output directory, records its hash and linked libraries, and
renders sample states with bounded execution times. It invokes only `--shots`,
which uses fixture data rather than performing installation actions.

## AArch64 results

The native test run passed 146 library tests and four binary tests, with no
failures or ignored tests. The release executable is an AArch64 ELF with SHA-256
`3a4f28c09e3e1251cc1dbf5b8d282a3ea17c46f8a45369ed1b86409c1957864f`.
All dynamic dependencies resolved in the Fedora 42 WSL build environment.

That initial local artifact used Fedora Rust 1.94.1. The environment has since
been corrected to use the repository-pinned Rust 1.97.1 through rustup. All 150
installer tests also passed with 1.97.1; its release rebuild and screen rerun
remain pending. Do not treat the initial binary hash as pinned-toolchain
release evidence.

All 16 screen states rendered at 1280×800, plus disk, account, and review at
800×600. This includes alongside-install, recovery, and failure states. The
800×600 review screenshot was inspected: the summary text is readable and the
page has a vertical scrollbar for content below the viewport.

These checks establish native execution and software rendering. They do not
establish input/scroll interaction, physical GPU operation, installation onto
a virtual or physical disk, or operation inside the final signed installer
image. Those remain separate acceptance requirements.
