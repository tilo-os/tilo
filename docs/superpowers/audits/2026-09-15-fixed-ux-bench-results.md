# Desktop UX bench: repaired matrix

## Result

**48/48 runs pass, 0 execution errors, 0 automated coverage gaps.** All eight original goals execute all six variants without increasing click/key budgets or lowering required outcomes/thresholds. Final report: `target/ux-bench/fixed-matrix/index.html`; machine-readable audit: `target/ux-bench/fixed-matrix/evidence-audit.json`.

150 checkpoints retain screenshots, inspection trees, resources and expected/observed checks. All 48 runs retain isolated profiles, permission-filtered initial action catalogues and shutdown-persistence evidence. They share executable BLAKE3 `2039fa6650feafb0a602ee9e1fc2254373fab68652943324b33353a0a5b93c04`.

| Goal | Variants passed | Actual route cost |
| --- | --- | --- |
| Find and open a document in Files | 6/6 | 1 clicks / 1 keys / 0 wheel |
| Fresh desktop discoverability | 6/6 | 0 clicks / 0 keys / 0 wheel |
| Scroll Files content without moving the workspace | 6/6 | 0 clicks / 0 keys / 5 wheel |
| Desktop navigation geometry | 6/6 | 0 clicks / 0 keys / 0 wheel |
| Desktop navigation geometry and contrast | 6/6 | 0 clicks / 0 keys / 0 wheel |
| Open Files from desktop | 6/6 | 0 clicks / 7 keys / 0 wheel |
| Reach workspace overview | 6/6 | 3 clicks / 0 keys / 0 wheel |
| Rename a workspace | 6/6 | 2 clicks / 8 keys / 2 wheel |

Files scroll measures exactly −60 logical pixels down and +60 back in every variant. Its real list viewport stays fixed, as do the workspace chip pixels. Browse verifies actual selection, native Editor identity and exact `Fixture document 0\n` contents. Files launch verifies its native provider and a visible seeded row. Settings reaches the General summary and returns to its name input before renaming; all six reopened profiles contain `Bench`.

## Rendered contrast

Threshold remains **4.5:1** for both explicit glyph/background pairs.

| Variant | Workspace name | App shortcut hint |
| --- | --- | --- |
| baseline | 9.71:1 | 15.13:1 |
| compact | 9.71:1 | 15.13:1 |
| light | 17.05:1 | 14.42:1 |
| high-contrast | 15.13:1 | 20.75:1 |
| retina | 9.71:1 | 15.13:1 |
| large-text | 9.71:1 | 15.43:1 |

Baseline/light/high-contrast/Retina retain original workspace `[164,25]` / `[150,35]` and hint `[553,599]` / `[330,591]` pairs. Compact uses hint `[275,591]` / `[272,590]`; large text uses workspace `[139,17]` / `[150,35]` and hint `[320,598]` / `[314,598]`. The replacement foreground pixels are inspected interiors of Home's H and the hint's A, excluding icons and borders; backgrounds are adjacent painted surfaces. `VARIANT_PIXEL_PROBES` in the runner records this authored mapping. The former compact/large-text pixel locations missed their glyphs; the failing evidence remains in `target/ux-bench/task3-matrix-first/` (46 pass, 2 probe failures).

## Visual checks and regressions

Inspected final compact document content; baseline, compact and high-contrast Files scrolling; light and 200% desktop contrast; and 960×540/200% settings before/after scrolling and after rename. The latter is retained at `target/ux-bench/task3-settings-compact-200-verified/`: `settings.png`, `settings-summary.png`, `renamed.png`. Topbar/Help no longer double-scale; Help's measured label stays inside its real hit target. General content clips to its scroll viewport, the summary is reachable, and hidden name controls reject projected accessibility focus/hits. Large-text header and tab labels stay within their allocated surfaces.

Verification passed: UX bench 18 tests, Scrollable 34, native shell binary 10, desktop regressions 42, and focused General-scroll/native-search/Files-bounds/content-viewport tests (one each). Python suite: 4 tests. Some focused filters overlap the broader suites. Unrestricted Clippy passes for UI and shell library/ux-bench targets. Logs and commands: `target/ux-bench/task3-validation/`. Linker emits its existing large-unwind-section warning; no introduced lint errors remain.

The normal-restart test previously retained an old store assertion handle while opening a second session, correctly producing a typed writer-lock failure. It now drops old driver/store/tree ownership before reopening; no production admission checks were bypassed.

## Reproduce and limits

```sh
cargo test --locked -p ui --lib ux_bench --no-default-features
cargo test --locked -p ui --lib widget::scrollable::tests --no-default-features
cargo test --locked -p shell --bin shell --no-default-features
python3 scripts/ux-bench/test_suite.py
cargo clippy --locked -p ui -p shell --lib --bin ux-bench --no-default-features
cargo build --locked -p shell --bin ux-bench --no-default-features
python3 scripts/ux-bench/run-suite.py --matrix --out target/ux-bench/next-fixed-matrix
```

Use a new output directory to preserve evidence. The original 6-pass/29-failure report remains `target/ux-bench/final-matrix/index.html`; its historical audit is `2026-09-15-ux-bench-results.md`.

Native hardware event delivery, macOS gesture arbitration, physical Retina raster quality and subjective ease still require live acceptance. These two pixel probes do not certify whole-screen contrast. Passing automated goals does not replace native-hardware review.

## Final review cleanup and native build

The final review's closed-document cache/reference cleanup is implemented and approved. A new open/close/restart regression passes; native tests remain 10/10, and fresh browse/scroll runs pass at `target/ux-bench/final-fix-browse-file` and `target/ux-bench/final-fix-files-scroll`. The 48-run matrix above predates this cleanup and remains preserved; covering journeys were rerun afterward.

The final shell-only Clippy check (`--no-deps`) passes. A subsequent unrestricted check encounters 28 lint errors in concurrently edited, unrelated UI form-composition code; the earlier unrestricted pass above describes the matrix build. Exact output remains in `.superpowers/sdd/2026-09-15-fix-desktop-ux-failures/final-fix-clippy.log`.

`bash shell/build-macos.sh` succeeded and produced `target/macos/qOS.app`; the existing unwind-section linker warning remains. No changes were committed or pushed, and the shared dirty worktree is preserved.
