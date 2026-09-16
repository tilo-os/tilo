# Task 3 report — Browse and form composition

Status: **DONE_WITH_CONCERNS**. The scoped Store, Settings, Themes, and VPN work is committed. Final acceptance still depends on the separately owned shared Card/ContentCard/CardGrid automatic minimum-height fix.

## Commit

- `d3591fe85` — `fix(apps): rebalance browse and settings layouts`

No push or merge was performed.

## Implemented

- Store: corrected fallback-artwork inset direction; rebuilt catalogue cards with intrinsic ContentCard composition and accessible Actions; kept selected filter values readable; reduced redundant heading/count layers; used one width decision for the masthead and catalogue; removed the duplicate 800x480 source summary; bounded one-column CardGrid sizing to the catalogue's real 1120px content width instead of converting `f64::MAX` to infinite `f32` geometry; retained typed Details/primary bindings and operation locks.
- Settings: reduced stacked title/body spacing; removed the repeated Accounts introductory card title while preserving account state and Add account; added first-action, Region ordering, and finite-geometry coverage. The invalid Region CardGrid workaround was removed after its infinite track poisoned rendering.
- Themes: added explicit local Customize/Full preview state for narrow layouts; hidden panes are removed from layout, drawing, events, and accessibility children; live presets begin on the full specimen; editable themes begin on controls; per-view scroll and edits survive resize/view changes; Escape returns from preview and restores focus to Full preview.
- VPN: derived the compact header bottom from the real title/control bands and reused the resulting layout rectangles for notices, rows, pointer, keyboard, and semantic paths.

## Verification

- `cargo fmt -p store -p settings -p themes -p vpn -- --check` — passed.
- `cargo build -p store -p settings -p themes -p vpn` — passed.
- `cargo test -p store --lib -- --skip full_narrow_catalogue_keeps_discover_section_below_ready_cards` — 102 passed, 1 deliberately skipped.
- `cargo test -p settings -- --skip region_preview_follows_every_format_control` — 334 passed across library, integration, persistence, mail-loop, and doc-test targets; 1 deliberately skipped.
- `cargo test -p themes` — 50 passed.
- `cargo test -p vpn -- --skip real_loopback_sys_bytes_are_readable_and_monotonic` — 54 passed, 1 pre-existing Linux-only test skipped on macOS.

The two shared-dependency regressions were rerun separately and fail for the expected reason:

- `cargo test -p store full_narrow_catalogue_keeps_discover_section_below_ready_cards -- --nocapture` — fails because ready-card descendants end at y=863 while `Discover more` begins at y=576.
- `cargo test -p settings --test it region_preview_follows_every_format_control -- --nocapture` — fails because `First day of week` ends at y=450 while `Preview` begins at y=436.

These failures reproduce the diagnosed zero automatic block-axis minimum contribution in the shared Card/ContentCard/CardGrid stack. No additional app height formula or shared primitive edit was added here.

## Fresh visual evidence

All paths below were captured from the scoped production commit with freshly built binaries:

- Store short-height header: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/store/short.png` — one source summary, title/actions use available width, catalogue begins materially earlier.
- Settings Accounts: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/settings/panel-external_accounts.png` and `panel-external_accounts-narrow.png` — correct route, compact heading, Add account visible.
- Settings Region: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/settings/panel-region_formats.png` and `panel-region_formats-narrow.png` — truthful Region route; the remaining field/Preview collision is visible and matches the red shared-dependency regression.
- Themes editable controls: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/themes/scroll-narrow.png` — gallery closed and Customize controls begin in the first viewport.
- Themes live preset: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/themes/live-narrow.png` — gallery closed and the live preset specimen is visible.
- Themes editable full preview: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/themes/preview-narrow.png` — custom Sunset specimen remains available through Full preview.
- VPN error state: `.superpowers/app-ui-2026-09-15/task3-resume-20260916/vpn/error-narrow.png` — header, Retry/Dismiss notice, and first connection row are disjoint.
- Earlier wide/compact/detail/import coverage remains in `.superpowers/app-ui-2026-09-15/task3-final-20260915/`.

Settings category slugs use underscores. Hyphen-named `panel-region-formats*` captures silently fall back to External Accounts and are not evidence; use only the underscore-named captures above.

## Remaining limitations

1. Store full-catalogue cross-section containment and Settings Region card ordering remain blocked on the shared automatic minimum-height fix described in `.superpowers/app-ui-2026-09-15/intrinsic-card-diagnosis.md`.
2. The VPN `/sys/class/net/lo` test remains unchanged and requires Linux validation by the controller.
3. Static captures do not prove interaction alone; the focused Store binding, Themes semantic/keyboard/Escape/edit-preservation, Settings geometry, and VPN pointer/keyboard tests provide the behavioral evidence.
