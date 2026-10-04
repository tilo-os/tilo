# Local changes: Parallels regression run, 2026-09-15

## Scope and environment

Tested the local macOS-shell work on macOS and in the **Fedora 44 ARM64**
Parallels VM, with Rust 1.97.1. The Linux renderer was Fedora's native llvmpipe
(LLVM 22.1.8), not the pinned x86-64 CI builder image. Builds used four jobs and
no debug information in the VM. Source was ultimately transferred as an immutable
archive and every regular file was checked against its SHA-256 hash after extraction.
Other tasks continued editing this checkout during the run; later working-tree
edits are separate from the tested snapshot.

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

Final Linux workspace build, all-target check, example harnesses, and targeted
regressions are recorded below after completion.
