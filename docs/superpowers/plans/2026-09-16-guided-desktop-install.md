# Guided Desktop Installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the guided Desktop choice install an authenticated Tilo session on supported Linux hosts while preserving the current desktop.

**Architecture:** Read-only host discovery selects an exact package from a catalog authenticated by the signed setup application. An elevated helper independently validates that choice, copies verified package bytes into private root-owned staging, and invokes the native package manager. The UI reports package-manager outcomes and directs the user to the existing login-session chooser.

**Tech Stack:** Rust, serde, reqwest, SHA-256, Linux package managers, existing Imager worker and elevation patterns.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- Desktop keeps the Linux installation and adds a Tilo session.
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
`.forgejo/workflows/release-desktop.yml` produces target-specific packages and
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

## Task 4: Guided Desktop installation UI

**Files:** Modify setup/view.rs and controller.rs; create focused
setup/desktop_flow.rs (or equivalent coordinator module) if needed to keep the
view free of process orchestration; extend existing screenshot scenarios/tests.

**Interfaces:** A Desktop flow coordinator consumes live readiness, verified
download and helper events; publishes an explicit UI state; retains the reviewed
selection and folder choices through cancellation/refusal/failure/retry. It
never treats a screenshot fixture as production capability evidence.

- [ ] Drive Desktop availability from the live readiness worker. Show the
  selected package version and host before authorization, and retain folder
  choices across install failure/retry. Recheck readiness when stale and keep
  unavailable options visibly explained.
- [ ] Wire verified download, helper progress and installed-session verification
  into the UI off its thread. Bound event handling; do not block on worker joins.
  Report download cancellation separately from a begun native transaction,
  whose terminal outcome must remain tracked when the view closes.
- [ ] After installation, explain how to select Tilo at the next login; do not
  log out the current desktop automatically or change its default. Matching
  already-installed packages still require the helper's session verification.
- [ ] Replace Desktop's fabricated /home/tilo destination estimates with explicit
  unresolved destination state or a real user-resolved, visibly reviewed
  destination. This task must not start copying before Task5 has resolved and
  authorized the actual target. Preserve the actual selected folders/modes for
  continuation rather than treating installation as migration completion.
- [ ] Exercise failed authorization, package-manager error, worker loss,
  download error/cancellation, retry and already-installed matching version
  through the actual coordinator/controller. Inspect compact and normal
  screenshots and keyboard navigation. Commit only reviewed integration hunks.

## Task 5: Durable first-login continuation and delivered import UI

**Files:** Add a focused setup/desktop_continuation.rs module; modify import_view.rs
and Task4 coordinator integration. Extend the actual host-package build/staging
and installed-payload checks if needed to deliver the import UI. Modify the
trusted shell login-completion/application-launch integration only at narrow
identified entry points. Record exact additional paths in the task brief before
dispatch; preserve concurrent shell/packaging edits.

**Interfaces:** A bounded private device-local continuation binds selected
source identities, modes and explicit destination intent to a reviewed setup
revision. The target-side import coordinator resolves real destinations and
uses the existing MigrationStore/MigrationWorker; it does not duplicate copy
logic. Persisted choices are not filesystem authority.

- [ ] Write failing tests for malformed/oversized state, platform/path mismatch,
  source substitution, missing source, replays, interrupted acknowledgement,
  duplicate launches, Later/per-folder overrides and completion before identity.
- [ ] Persist only secret-free migration continuation. Identity setup remains
  in its trusted first-login boundary. Never provision identity from a host app
  window, start import before trusted completion, or export migration state
  into collaborative documents.
- [ ] Resolve the real current-user Tilo destination, show source/destination and
  collisions, and begin Copy only after both reviewed endpoints are resolved
  and revalidated. Keep local Copy distinct from Connected. A source edit,
  unavailable mount or destination substitution must remain a pending/error
  outcome and cannot silently advance the plan.
- [ ] Resume the reviewed folder list using existing durable copy jobs. Retain
  per-folder terminal outcomes, skips and conflicts. Reopening continuation must
  not duplicate completed work or overwrite destination edits. Decide later
  remains accessible through the later Add folders surface.
- [ ] Deliver the actual import UI/service in host packages through the existing
  supported application path or a narrowly documented native integration. The
  currently built package has only shell/launcher/unlock-helper; a development
  binary or missing executable is not a production handoff. Add meaningful
  package content/startup checks and preserve the existing session contract.
- [ ] Exercise actual coordinator-to-continuation-to-copy tests, inspect compact
  and normal first-login/import UI captures, and commit scoped changes. Root
  coordinates native builds, package mutation and graphical sessions.

## Task 6: Native graphical lifecycle and release evidence

**Files:** Extend docs/regression/2026-09-16-guided-setup.md and native host package
verification scripts. Production catalog entries are separate authenticated
release artifacts and remain absent until their evidence is genuine.

- [ ] Rebuild and install the real final package in disposable supported guests.
  Log into Tilo through the display manager, finish trusted identity setup,
  continue the selected real folder import and verify source/destination bytes.
- [ ] Log out and return to the original desktop. Confirm prior session files,
  default selection, portal environment and original files still work. Retain
  per-distro/architecture evidence; a headless startup test alone is insufficient.
- [ ] Exercise native failed authorization and resumed migration, plus delayed
  source availability. Record actual package digest/version and final helper/UI
  build identity so evidence cannot be attached to a different payload.
- [ ] Populate release evidence only from completed native runs and reviewed
  authenticated artifacts. Enable only supported exact combinations; never
  fabricate runtime evidence to bypass the catalog gate.
- [ ] Run the final Desktop component review, including previously deferred
  catalog/download/folder lint findings and HTTP fixture flakes. Apply its one
  scoped fix wave and re-review; preserve the full Windows/FullOS/Connected goal.

## Coverage boundary

This plan implements the Linux Desktop installation route. Internal Full OS
staging, retained-OS boot, disk allocation, cross-reboot source mapping and
Windows delivery remain separate required work. Folder copying and durable
resume use `2026-09-16-guided-folder-copy.md`; two-way synchronization additionally
requires its own baseline, conflict, deletion-retention and mount-safety work.
