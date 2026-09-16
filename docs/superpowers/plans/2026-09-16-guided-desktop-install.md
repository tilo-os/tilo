# Guided Desktop Installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the guided Desktop choice install an authenticated qOS session on supported Linux hosts while preserving the current desktop.

**Architecture:** Read-only host discovery selects an exact package from a catalog authenticated by the signed setup application. An elevated helper independently validates that choice, copies verified package bytes into private root-owned staging, and invokes the native package manager. The UI reports package-manager outcomes and directs the user to the existing login-session chooser.

**Tech Stack:** Rust, serde, reqwest, SHA-256, Linux package managers, existing Imager worker and elevation patterns.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- Desktop keeps the Linux installation and adds a qOS session.
- Do not present an unavailable Desktop choice as actionable.
- Backend revalidation remains required immediately before privileged operations.
- Source deletion is never part of a one-time copy.
- No collaboration sharing or upload permission follows from migration.
- This subsystem does not authorize disk changes or satisfy the Full OS route.

## Existing boundaries

`dist/session/README.md` defines the installed session and first-login contract.
`docs/install-contract.json` names Ubuntu 24.04, Debian 13, Fedora 44, Arch, and
NixOS package targets. Package availability alone is not a successful graphical
session test. Preserve separate distro support evidence in the setup catalog.

`apps/imager/src/release.rs` authenticates the media catalog by embedding it in
the signed application; its decompression path is specific to disk images.
`.forgejo/workflows/package-desktop.yml` produces target-specific packages and
release checksums. A checksum fetched beside a package is not an independent
trust anchor. Embed reviewed release metadata at build time; never activate a
route from an unverified network manifest.

`apps/imager/src/write.rs` has a hidden elevated helper dispatched before GUI
startup. Reuse the protocol pattern, not its raw-device request type or target
selection. Desktop installation has no reason to obtain a block-device handle.

## Task 1: Typed desktop catalog and verified package cache

**Files:** Create `apps/imager/src/setup/desktop_catalog.rs`,
`desktop_download.rs`, `apps/imager/desktop-catalog.json`, and
`apps/imager/packaging/build-desktop-catalog.py`; modify setup/mod.rs and
release packaging only where required to embed the artifact.

**Interfaces:** `DesktopRelease` has id, version, distro_id, distro_version,
architecture, package_format, HTTPS URL, byte_size, sha256 and verified_runtime
fields. `parse_catalog(&str) -> Result<Vec<DesktopRelease>, String>` rejects
unknown schemas, duplicate selectors, invalid sizes/digests and untrusted URLs.
`download_package(&DesktopRelease, &Path, &AtomicBool) -> Result<PathBuf, String>`
returns only a fully verified file, never a partial-download pathname.

- [ ] Write failing parser tests with actual serialized catalogs: duplicate
  Fedora/44/aarch64 entries, invalid digest, HTTP URL, cross-origin URL, zero
  length, unknown format and the explicit unreleased empty catalog.
- [ ] Use a typed enum for formats Rpm, Deb, Arch and NixClosure. Deserialize with
  unknown-field rejection. Match platform selectors exactly; do not substitute
  a similarly named distribution or its ID_LIKE value.
- [ ] Keep the checked-in catalog empty and explicitly unreleased. The packaging
  generator consumes locally verified release artifacts and emits their actual
  digests/sizes; no invented URLs, versions or hashes.
- [ ] Exercise package download through a local test HTTP server: interrupted
  transfer, ignored Range, inconsistent Content-Range, oversized response and
  digest mismatch. Reuse existing bounded transport helpers if extraction is
  small; retain all existing media-download tests.
- [ ] Download as an ordinary user, check size and digest, and publish into a
  content-addressed cache using exclusive temporary files. Revalidate existing
  cached content before returning it. Never mark downloaded bytes executable.
- [ ] Run `cargo test -p imager --lib setup::desktop` and packaging-generator
  tests. Record results and commit only the task files.

## Task 2: Read-only Linux desktop readiness

**Files:** Create `apps/imager/src/setup/desktop_probe.rs`; modify setup/mod.rs.

**Interfaces:** `DesktopHost` records exact os-release ID/VERSION_ID, native
architecture, package-manager kind, immutable-host status and sandbox status.
`desktop_readiness(&DesktopHost, &[DesktopRelease]) -> DesktopReadiness`
returns an exact supported release or user-facing blockers, without executing
package managers or changing the machine.

- [ ] Start with a parser test that protects the important rejection:

```rust
#[test]
fn derivative_does_not_inherit_fedora_support() {
    let host = parse_os_release("ID=example\nID_LIKE=fedora\nVERSION_ID=44\n").unwrap();
    assert_eq!(host.id, "example");
}
```

- [ ] Add quoted-value, missing VERSION_ID, duplicate key, malformed input and
  architecture mismatch tests. Parse os-release as data; never source it in a
  shell. Map Rust architecture to catalog architecture explicitly.
- [ ] Probe native package-manager binaries, display-manager session support,
  existing package status, sandbox confinement and immutable deployment markers
  off the UI thread. Missing/ambiguous evidence yields a blocker.
- [ ] Initially attest ordinary supported package-manager hosts only. An ostree
  system requires a distinct validated layering/reboot adapter; never use
  `usroverlay` as production installation. NixOS requires a reviewed declarative
  integration path; do not run imperative package commands there.
- [ ] Test readiness against real parsed fixtures and the catalog, including a
  nominally supported host with no authenticated release and one whose release
  lacks verified_runtime evidence. Neither may enable Desktop.
- [ ] Run focused tests and a read-only native Fedora probe. Commit the module
  with captured non-secret probe evidence.

## Task 3: Verified native package installation helper

**Files:** Create `apps/imager/src/setup/desktop_install.rs` and
`desktop_worker.rs`; modify main.rs helper dispatch and setup/mod.rs.

**Interfaces:** A versioned `DesktopInstallRequest` contains only the selected
catalog release id and package input path. The helper receives the request digest
as an argument, resolves its own embedded catalog, re-probes the host, and reports
Preparing, Installing, Verifying, Complete or Failed through the worker protocol.
A complete outcome includes installed version and the verified session entry.

- [ ] Write refusal tests for unknown release ids, changed requests, wrong-host
  packages, altered cache bytes, truncated packages and unsupported sandboxes.
  Assert no package-manager invocation occurs on refusal.
- [ ] Pin the package input handle, reject nonregular files, stream it into
  exclusive private root-owned staging and verify the copied digest/size. Invoke
  the package manager only with the staged path after this check.
- [ ] Build argument vectors directly: no interpolated shell, no arbitrary
  flags, no --nogpgcheck, no dependency bypass, no key-trust auto-acceptance.
  Preserve the host's configured package-signature policy. Provide distinct
  fixed adapters for dnf, apt-get and pacman rather than guessing from names.
- [ ] Elevate only the hidden helper using the host authorization UI. Report
  cancellation/refusal distinctly. Once a package transaction begins, do not
  kill it to implement UI cancellation; keep tracking its terminal outcome.
- [ ] After exit, query the installed package version, validate the package-owned
  session entry and wrapper, and report completion only if both agree. A lost
  channel or nonzero package exit never becomes success. Preserve diagnostic
  output without exposing credentials.
- [ ] Test command construction and protocol handling using a recording process,
  then run the actual helper/package manager in a disposable supported guest
  with a locally built package. Preserve the guest's prior desktop. Record the
  native package database and session-entry results; do not infer graphical
  login success from package installation.
- [ ] Run focused tests and Clippy; commit reviewed task files.

## Task 4: Guided UI, login continuation and target evidence

**Files:** Modify setup/view.rs and controller integration; extend
`docs/regression/2026-09-16-guided-setup.md` and host package verification scripts.

- [ ] Drive Desktop availability from the live readiness worker. Show the
  selected package version and host before authorization, and retain folder
  choices across install failure/retry.
- [ ] Wire verified download, helper progress and installed-session verification
  into the UI. After installation, explain how to select qOS at the next login;
  do not log out the current desktop automatically or change its default.
- [ ] Pass only secret-free migration continuation to the qOS session. Identity
  setup remains in its trusted first-login boundary. Copy starts only when the
  reviewed source and qOS destination are both resolved.
- [ ] Exercise failed authorization, package-manager error, worker loss and
  already-installed matching version through the actual controller. Inspect
  compact and normal screenshots and keyboard navigation.
- [ ] In supported native guests, log into qOS, finish identity setup, import a
  real selected folder, log out and return to the original desktop. Confirm
  source bytes and the prior session still work. Retain evidence per distro and
  architecture; enable only combinations with completed evidence.

## Coverage boundary

This plan implements the Linux Desktop installation route. Internal Full OS
staging, retained-OS boot, disk allocation, cross-reboot source mapping and
Windows delivery remain separate required work. Folder copying and durable
resume use `2026-09-16-guided-folder-copy.md`; two-way synchronization additionally
requires its own baseline, conflict, deletion-retention and mount-safety work.
