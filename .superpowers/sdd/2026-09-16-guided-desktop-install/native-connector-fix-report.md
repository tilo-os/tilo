# Native connector eligibility fix report

Status: implementation and focused native regression verification complete. Real GDM login, package rebuild/install, and graphical lifecycle proof remain controller-owned follow-up work; these pure tests do not establish graphical login success.

## Source identity and scope

- Assigned base: `1e2c78e35e5ced0870182d5b33bb8d2c95aae86d`.
- Stable implementation commit: `ccc2ebae07b951367aef4888de564f888d5755a8` (`fix(drm): admit typed virtual scanout connectors`).
- Owned source: `shell/src/compositor/backend/drm/scanout.rs` only.
- Committed source SHA-256: `57cdffb88b9b0bf2ac4fa0d6b796dee7a8dd6eb2d5c055f190329648ad32103d`.
- Source diff: 64 insertions, 6 deletions. No guest mutation, package build, install, push, or merge was performed by this task.

The production failure is at the common connector discovery boundary. `CardHandle::connected()` used a lowercase connector-name substring filter that rejected both `Writeback-*` and `Virtual-*`. The actual Parallels KMS sink is `Virtual-1`, reports `Connected`, and has usable modes. The DRM dependency exposes `connector::Interface::Virtual` and `connector::Interface::Writeback` as distinct typed interfaces, so a name filter collapsed a real scanout sink with a capture-only connector.

The runtime's `configured_connectors()` consumes `CardHandle::connected()` for startup, hotplug reconciliation, and Settings capability publication. No second shared policy copy required a change. The unused single-output convenience constructor is not part of that configured runtime path.

## TDD evidence

The temporary RED commit was `298b833a1bfb946b3ff578a3c8b3226b2493362a`; its `scanout.rs` SHA-256 was `3a4690fa9930d0669ae6eb892ff0d7cfe9f913247791b4ecdf7fb7dd41878538`. It extracted the existing string policy without changing its decisions, then exercised that actual production helper with five focused cases.

Native RED command from the Fedora build target:

```text
CARGO_TARGET_DIR=/home/parallels/qos-target cargo test --locked -p shell --lib --no-default-features scanout_eligible -- --nocapture
```

Result: exit `101`; 5 selected, 3 passed, 2 failed, 2,705 filtered. The valid connected `Virtual` case failed because the old policy returned false. The typed `Unknown` case failed because the name policy returned true. Disconnected, no-usable-mode, and `Writeback` cases passed, showing those existing safeguards before the correction.

The minimal GREEN change uses `connector::Interface`: `Virtual` remains eligible, while `Writeback` and `Unknown` are ineligible. `State::Connected` and the presence of a mode chosen by `pick_configured_mode` remain mandatory.

Native GREEN command at implementation commit `ccc2ebae07b951367aef4888de564f888d5755a8`:

```text
CARGO_TARGET_DIR=/home/parallels/qos-target cargo test --locked -p shell --lib --no-default-features compositor::backend::drm::scanout::tests -- --nocapture
```

Result: exit `0`; all 20 selected scanout unit tests passed, 0 ignored, 2,690 filtered. This includes all five eligibility regressions and existing connector-key/configuration, cursor, damage, mode-timing, and page-flip policy regressions. Compilation took 1m31s and test execution 0.34s. The only warning was the pre-existing unused `restore_documents` method in shell hosted-input code, outside this task's scope.

## Formatting and self-review

- `rustfmt --edition 2024 --check shell/src/compositor/backend/drm/scanout.rs`: passed after applying its one-line expression layout.
- `git diff --check -- shell/src/compositor/backend/drm/scanout.rs`: passed before commit.
- `cargo fmt --all --check`: did not pass because concurrent Browser work is not rustfmt-clean in `apps/browser/src/browser_tabs.rs`, `browser_widget.rs`, `site_controls.rs`, and `main.rs`. Those files were not modified or staged. The owned source passed the scoped rustfmt check above.
- A broad shell Clippy run was not performed: it would compile the entire shell rather than a bounded connector policy, and the controller did not require it to block the focused native review. The focused Linux test build compiled the changed code successfully.

Self-review found the eligibility helper is called directly by the single shared discovery path. It admits a connector only when state is exactly `Connected`, a usable live mode exists, and the typed interface is a known scanout sink. Typed `Writeback` remains excluded and `Unknown` is not treated as evidence of usability. Persisted `enabled` configuration is still applied afterward. `bring_up()` still requires an available CRTC, and `DrmScanout::from_card()` still has to allocate buffers and complete the real modeset. DRM seat access, permissions, card acquisition, and all later presentation gates are unchanged.

## Remaining concerns and handoff

The regression proves only the connector policy. It does not prove that Parallels accepts the later CRTC/modeset path or that GDM reaches the qOS identity screen. The controller must rebuild the production compositor/package from the stable source commit, record a fresh artifact digest, install only in the marked clone under the separately defined package-label policy, and repeat the real GDM login plus graphical lifecycle proof. `Writeback`, missing modes, disconnected state, disabled configuration, missing CRTC, modeset failure, and seat/permission failure remain refusal paths by design.

The controller's first production release-build attempt after the passing tests exited `101` before compilation because the concurrently edited workspace `Cargo.lock` requires an update while the build uses `--locked`. The controller made no source or lock change. This is an external workspace-integration prerequisite for the package rebuild, not a passing graphical result or a failure in the connector source.
