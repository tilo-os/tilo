# Local changes: Parallels regression run, 2026-09-15

## Scope and environment

Tested the local macOS-shell work on macOS and in the **Fedora 44 ARM64**
Parallels VM, with Rust 1.97.1. The Linux renderer was Fedora's native llvmpipe
(LLVM 22.1.8), not the pinned x86-64 CI builder image. Builds used four jobs and
no debug information in the VM. Source was ultimately transferred as an immutable
archive and every regular file was checked against its SHA-256 hash after extraction.
Other tasks continued editing this checkout during the run. The publication
branch `codex/macos-shell-regression-20260915` freezes the tested snapshot on
base `a475555f4`; subsequent commits and working-tree edits in the original
checkout are separate from this publication.

## Full Linux workspace run

`cargo nextest run --workspace --locked --profile ci` built successfully and ran
**16,555 tests across 284 binaries**: **16,521 passed, 34 failed, 46 skipped**.
Five passing tests needed a retry; five were reported as leaky. The test run took
22m 08s after its 13m 42s compilation. This was not an all-green run.

### Corrections made during verification

- Restricted the new native swipe event and its consumers/tests to macOS. Its
  unconditional addition broke Smithay's exhaustive Linux event match.
- Added Scratchpad's missing package manifest and integrated its coordinated
  state/root lifecycle with `LaunchSpec`, preserving explicit Save. Added a real
  CLI screenshot regression for viewport and text-scale options.
- Updated the Settings navigation test fixture to fit the full sidebar with
  correct intrinsic gaps, and asserted target visibility before pointer clicks.
  The previous fixture clicked About at y≈1545 in a 1280px viewport.
- Made the styling audit recognize explicit file-level test gating, with a
  regression test, and marked the renderer fixture as test-only.
- Corrected formatting and narrow Clippy findings in the new layout/host code.

### Failures resolved by environment or isolated reruns

- Both logind inhibitor tests pass with the required VM privileges.
- Both nearby personal-device enrollment tests pass after temporarily opening
  their UDP discovery port, 45831, through Fedora's firewall. The firewall rule
  expires automatically; the firewall was not disabled.
- Both application-index readiness tests and the 4K framebuilder staging test
  pass when run in isolation.
- The video-editor render test passed Nextest's retry.

### Remaining findings from the broad run

- Eight visual baseline tests failed: two ebook typography tests, five shell
  `e2e_shell_flows` snapshots, and the UI multilingual typography matrix.
  Some shell differences are small pixel deltas, but the typography matrix also
  changes dimensions. These cannot all be dismissed as renderer variation.
  Baselines were not regenerated on the Fedora VM.
- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` flags
  existing `frame.rs` night-light/color-filter constants (one RGB constructor,
  two white operands). Those transforms and the audit baseline were not changed.
- `ush::serve_handoff::a_terminal_gets_a_working_shell_over_the_service_socket`
  fails when the guest layer cannot access
  `usr/lib/sysimage/libdnf5/packages.toml` on this Fedora VM.

## macOS verification

- UI library: 2,966 passed, 2 ignored in the initial run.
- Shell library: 2,332 passed, 2 skipped (one leaky passing test) in the initial run.
- Native swipe regressions: 3 passed after the platform-scope correction.
- Runtime filesystem: 85 passed; one integration test and one doctest ignored.
- Settings integration: all 30 passed after correcting the navigation fixture.
- Scratchpad: all 7 integration tests passed, including the new CLI regression.
- Native shell runner: all 9 current tests passed, including hosted Files
  semantics, scrolling, document opening, and missing-document restoration.
- Python tooling: 15 script tests, 58 tool tests, and 3 UX-suite tests passed.
- Scratchpad Clippy with `-D warnings`: passed after launcher integration.

## Desktop UX evidence

Initial baseline run: **1 passed, 7 failed**.
Later run after concurrent Files fixes: **3 passed, 5 failed**.
Passing: document browsing/opening, Files scrolling, desktop navigation geometry.
Remaining: desktop discoverability, navigation contrast, opening Files from the
desktop, reaching overview, and workspace renaming.

The later report is `target/ux-bench/regression-final-20260915/index.html`.
These runs inject input into the production widget tree; they do not establish
physical trackpad/mouse delivery or full OS-image boot acceptance.

## Final snapshot checks

The default GNU runtime shared-library link was stopped after more than 14
minutes of CPU-active linking. Final verification builds workspace applications
with `cargo build --workspace --locked --exclude runtime-dylib`, then builds the
shared runtime separately with `cargo rustc --locked -p runtime-dylib --lib --
-C link-arg=-fuse-ld=lld`. Both build commands passed, as did
`cargo check --workspace --locked --all-targets`.

The first example-harness run failed two 4K redraw tests with GPU fence timeouts
(52 passed, 2 failed, 7 ignored in that binary). The full serial rerun,
`cargo test --workspace --examples --locked -- --test-threads=1`, passed:
**69 passed, 0 failed, 7 ignored**. Both timed-out 4K cases passed.
The final targeted Nextest run completed **6,082 tests: 6,076 passed, 6 failed,
521 skipped**, with four test threads and no retries (8m 17s). It covered UI,
shell, runtime-fs, Settings integration, Scratchpad, and package manifests.
The corrected packaging, Settings, Scratchpad and runtime-fs tests passed.
The six failures were:

- `app::viewport_composition_tests::review_active_trap_retains_keyed_selection_after_replacing_and_reordering_controls`
- `app::viewport_composition_tests::review_failed_headless_resize_can_return_to_last_successful_size`
- `app::viewport_composition_tests::review_native_views_mount_only_after_successful_final_composition`
- `widget::layout::tests::review_disabled_duplicate_target_does_not_transfer_focus_to_enabled_other`
- `bound_component_dispatch::legacy_bound_mutable_admission_edits_only_its_record_and_refuses_missing_or_busy`
- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow`

The first four are review regressions captured while another task was working.
That task subsequently committed lifecycle fixes in `367aecb64`; that newer
commit is outside this tested snapshot. The dispatch test observes an extra
callback with an empty record. These failures are retained, not suppressed.
The final snapshot is **not all green**.

A separate read-only review of the regression fixes found no concrete
correctness regressions.

The new Linux shell also passed the repository headless smoke loop: its control
socket started, 41 targets were exposed, the workspace control responded, and
palette/final screenshots were rendered.

## Local evidence

Host logs: `/tmp/qos-regression-20260915/`.
VM logs: `/home/parallels/qos-regression-20260915/`.
Copied VM logs and immutable source manifest:
`/Users/user/.cache/qos-regression-20260915/`.
