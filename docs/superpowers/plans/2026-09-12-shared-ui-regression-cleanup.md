# Shared UI Regression Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore meaningful search-pagination coverage and theme-token conformance while preserving the shared app behavior on macOS and Linux.

**Architecture:** Exercise the existing bounded search page API for late exact matches, and use the existing on-surface ink computation for two accent-filled controls. No new platform abstraction is needed for these failures.

**Tech Stack:** Rust, shared `ui` search/theme APIs, Notes/Files port prerequisite, nextest.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, sections 6 and 11.

## Global Constraints

- Keep shared app logic; host conventions belong in the framework.
- Every required failure remains a delivery blocker.
- Preserve bounded interactive search work and complete paged discovery. Do not make `matching_ids` perform an unbounded scan to satisfy a stale suffix-index test.
- Do not increase or regenerate design-token baseline counts. Fix token consumers.
- Work in `/Users/user/dev/qos-macos-shell`, branch `codex/macos-shell`. Use `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell` on Mac. The controller runs the same focused tests on Fedora after git sync to the exact reviewed commit.
- Full completion still requires the Mac shell hosting real apps, the complete app capability map, Fedora headless and all applicable tests; these tasks resolve only recorded prerequisites.

## Task 1: Verify late search matches across bounded pages

**Files:**
- Modify/test: `ui/src/search/provider.rs` (index documentation and the stale suffix-range test).

**Interfaces:**
- Consumes `SearchTextIndex::matching_page(query, after, work_budget, result_limit) -> SearchTextPage`, with `ids`, `next_after` and `inspected`.
- Preserves `matching_ids` as a single bounded page and all production search code.

The old test expects one call limited to 256 complete candidates to find record
296 after 296 false trigram candidates. Its assumption belonged to a previous
suffix index. The current public API explicitly provides a continuation cursor;
production catalogues use paging. The requirement is eventual exact discovery
with bounded work, not unlimited work in a legacy convenience method.

- [ ] Reproduce the existing failure: `cargo nextest run -p ui --lib --locked --profile ci -E 'test(exact_suffix_range_does_not_lose_late_matches_to_false_candidates)'`. Record the failure `[]` versus `[296]`; use the designated native target.
- [ ] Replace that test with the following test of the documented public contract:

```rust
#[test]
fn bounded_pages_find_late_exact_matches_after_false_candidates() {
    let mut texts = vec!["abcXbcd"; 296];
    texts.push("the exact abcd resource");
    let index = SearchTextIndex::new(texts);

    let first = index.matching_page("abcd", None, 256, 1);
    assert!(first.ids.is_empty());
    assert_eq!(first.inspected, 256);
    assert_eq!(first.next_after, Some(255));

    let second = index.matching_page("abcd", first.next_after, 256, 1);
    assert_eq!(second.ids, vec![296]);
    assert_eq!(second.inspected, 41);
    assert_eq!(second.next_after, None);
}
```

- [ ] Replace the stale paragraph claiming each suffix is stored with this accurate documentation:

```text
Posting lists narrow the candidates; complete-text verification rejects false
gram combinations. Each page bounds verification work and supplies a cursor
when candidates remain. Embedded NULs and all Unicode scalar values retain
their ordinary string semantics.
```

- [ ] Format the file and run `cargo nextest run -p ui --lib --locked --profile ci -E 'test(search::provider::tests::)'`. Keep all existing rare-late-match, Unicode/NUL, work-budget, result-budget and continuation assertions. Do not change production algorithms.
- [ ] Run `cargo clippy -p ui --lib --locked -- -D warnings` with the designated target. Commit only `ui/src/search/provider.rs` as `test(ui): cover late exact matches through bounded search pages`.
- [ ] Controller verifies the same search group on Fedora. Report this as a correction of stale coverage, not a production search algorithm fix.

## Task 2: Use shared ink for new accent-filled app controls

**Files:**
- Modify: `apps/ebook/src/app.rs` (shortcut-capture pill label).
- Modify: `apps/totp/src/app.rs` (persistence Retry label).
- Existing verification: `ui/tests/design_token_audit.rs` (leave unchanged).
- Modify only the already-reduced Calendar row in `ui/tests/app_shell_design_token_baseline.txt`, as specified below.

**Interfaces:**
- Consumes `Theme::on_surface(surface: Color) -> OnCard`, selecting `.ink` against the actual painted background.
- Keeps both widgets' actions, geometry, labels, fill, state and event handling unchanged.

Current full UI evidence shows ebook `literal_color` 27 versus baseline 26 and
totp `white_or_black` 2 versus baseline 1. The newly added capture pill and Retry
label introduced those increments. Their background is `theme.accent_primary`.

- [ ] Reproduce the existing audit failure with `cargo nextest run -p ui --locked --profile ci -E 'test(app_and_shell_theme_escape_hatches_do_not_grow)'` on the designated native target.
- [ ] In ebook's `draw_shortcuts_settings` capture-pill text color, replace only the capturing branch:

```rust
if capturing {
    theme.on_surface(theme.accent_primary).ink
} else {
    c.text_primary
}
```

  The unique target is the `Press a key…` shortcut-capture pill whose current branch uses `Color::from_rgb8(250, 250, 252)`.

- [ ] In totp's `draw_text_weighted` call for the literal `"Retry"`, replace its `Color::WHITE` argument with `theme.on_surface(theme.accent_primary).ink`. Preserve the older Add-account control and all other styling.
- [ ] Format only the two changed source files. Run the complete design-token audit binary: `cargo nextest run -p ui --locked --profile ci -E 'test(design_token_audit::)'`. Both counts must return to their existing baselines; never set `UPDATE_APP_SHELL_DESIGN_BASELINE`.
- [ ] Run `cargo check -p ebook -p totp --lib --locked` using the designated native target. This validates both consumers; report any unrelated platform closure failure rather than expanding the patch.
- [ ] Commit only the two app files as `fix(apps): use theme ink for accent control labels`. No new behavior-mirroring test is needed for these direct shared-token substitutions; existing theme contrast tests own the helper.
- [ ] Controller verifies the audit and both app-library checks on Fedora at the exact commit.

### Task 2 amendment: retain Calendar’s existing reduction

The complete audit now reaches its stale-entry check and shows Calendar topbar
white/black literals already fell to zero in existing commit `c21e19744`. The
original instruction to leave every baseline entry unchanged conflicts with the
audit’s explicit downward-ratchet contract. Do not edit its counting logic or
restore old literals. Change exactly this baseline line:

```diff
-apps/calendar/src/topbar.rs white_or_black 3
+apps/calendar/src/topbar.rs white_or_black 0
```

Leave every other baseline line untouched. Do not invoke the regeneration flag.
Run the complete four-test design-token audit module again and require all four
to pass. Commit the single baseline edit as `test(ui): retain calendar theme debt reduction`.
The two app-library checks already passed and need no repeat for this data-only
follow-up. The reviewer reviews the original task and this amendment together;
the controller runs both-host audit checks at the final task SHA.

## Review boundary

Task 1 must preserve the complete corpus and test eventual exact discovery while
respecting per-page limits. Task 2 must fix actual token consumers and preserve
the audit. Remaining font, GPU, host lifecycle and platform-service work remains
part of the full port objective.
