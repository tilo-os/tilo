### Spec Compliance

- ❌ Issues found. The measured-layout and shared-menu implementation meets most of Task 4, but painted bounds are absent from exported synthetic accessibility nodes (`ui/src/widget/toolbar.rs:833`, `ui/src/widget/toolbar.rs:857`), and popup pointer state permits wrong-target activation and stuck capture (`ui/src/widget/toolbar.rs:674`, `ui/src/widget/toolbar.rs:686`, `ui/src/widget/toolbar.rs:715`). These are implementation gaps, not plan-mandated defects.
- ✅ Scope matches the brief: the production diff changes only Toolbar and ContextMenu, uses the existing Widget hooks, preserves original callback ownership, and keeps the opt-in anchored menu separate from legacy point placement (`ui/src/widget/toolbar.rs:310`, `ui/src/widget/toolbar.rs:378`, `ui/src/widget/context_menu.rs:125`).
- ⚠️ Live macOS focus and assistive-technology behavior are not established by this diff or the rendered fixtures. The controller reports both retained fixtures pass its visual inspection; that is fixture evidence, not live OS acceptance (`ui/src/widget/toolbar.rs:1595`).

### Strengths

- `ui/src/widget/toolbar.rs:169`, `ui/src/widget/toolbar.rs:227`: renderer-measured extents consistently drive intrinsic sizing, fit, and placement; the chip is reserved and clamped for tiny allocations, and hidden rectangles are cleared.
- `ui/src/widget/toolbar.rs:310`, `ui/src/widget/toolbar.rs:378`: meaningful separators and original-index mapping reuse ContextMenu without cloning command callbacks; selection is drained once.
- `ui/src/widget/context_menu.rs:150`, `ui/src/widget/context_menu.rs:198`: the anchored route shares measured row geometry across clipping, scrolling, keyboard focus, and hit testing. The legacy-placement regression checks switching back to `open_at` (`ui/src/widget/context_menu.rs:1031`).
- `ui/src/widget/toolbar.rs:1388`: callback tests exercise every command through pointer, keyboard, and semantic activation. The admitted shared-layout helper and retained GPU fixtures provide stronger evidence than geometry seeded by hand.

### Issues

#### Critical (Must Fix)

- None.

#### Important (Should Fix)

1. **Export actual synthetic accessibility bounds.** `ui/src/widget/toolbar.rs:833` and `ui/src/widget/toolbar.rs:857` create command/overflow/MenuItem nodes without `with_bounds`; the menu node at `ui/src/widget/toolbar.rs:868` also has none. Implementing `synthetic_accessibility_rect` at `ui/src/widget/toolbar.rs:880` does not populate platform node bounds: `ui/src/accesskit_bridge.rs:893` calls `apply_synthetic_bounds`, which reads only `AccessNode.bounds` (`ui/src/accesskit_bridge.rs:961`). The host uses the rectangle hook for fallback click dispatch (`ui/src/app.rs:5762`), not tree export. Consequently the platform tree has named actionable commands but no painted locations, contrary to the binding requirement. Populate the AccessNode bounds from the same button/menu/clipped-row geometry and represent scrolled-out rows consistently. Add a regression asserting emitted synthetic node bounds (including a clipped/scrolled row), rather than only calling the rectangle hook as the existing test does (`ui/src/widget/toolbar.rs:1567`).

2. **Refresh pointer hover when routing through or closing the popup.** `ui/src/widget/toolbar.rs:674` forwards CursorMoved to ContextMenu and returns before updating Toolbar.hovered; the next toolbar press blindly copies that old target at `ui/src/widget/toolbar.rs:715`, and release activates it at `ui/src/widget/toolbar.rs:738`. Concrete sequence: hover visible command A, use keyboard arrows/Space to open overflow, move into the popup and click hidden command B, release, then click again at that same window position without moving. B closes the menu, but Toolbar.hovered still points to A. The second click invokes A even though the pointer is outside its rectangle. Opening by pointer instead leaves the stale Overflow target and can unexpectedly reopen the menu. This is wrong-target activation, not merely a stale highlight; intervening layout retains the target if it remains reachable. Track the current pointer through popup events and re-hit-test/clear hover on close before accepting another press. Add the described no-CursorMoved-between-clicks regression, verifying no unrelated callback fires.

3. **Match the release-suppression flag to the actual mouse button.** `ui/src/widget/toolbar.rs:686` sets swallow_release on every mouse-button press, while only a left release clears it (`ui/src/widget/toolbar.rs:722`). Concrete sequence: open overflow, right-press and right-release, then Escape. ContextMenu ignores those right-button events (`ui/src/widget/context_menu.rs:642` handles left presses only), the right release leaves the flag set, Escape closes the menu, and `wants_pointer_capture` stays true (`ui/src/widget/toolbar.rs:801`). Row then exclusively routes events to Toolbar instead of siblings (`ui/src/widget/row.rs:287`), until an unrelated left release finally clears the flag; the user's next left click can be lost. Restrict suppression to the left press that needs dismissal/selection release suppression, or explicitly track and clear the corresponding button. Test right/middle press-release followed by Escape and assert capture ends and the next sibling click succeeds.

#### Minor (Nice to Have)

- Known baseline only: the implementer reports the macOS `__eh_frame section too large (max 16MB)` linker warning in successful lib-test runs. Per the controller's supplied baseline determination, this is not a new Task 4 defect and does not require a task fix.

### Assessment

**Task quality:** Needs fixes.

**Reasoning:** The core measurement and overflow implementation is coherent and substantially tested. Exported accessibility geometry and two concrete pointer-state transitions remain incorrect; fix those before approving Task 4.

### Test and Source Checks

- Reviewed the supplied brief, preflight, implementer report, and package for effective base `d02c1ade3` through head `1b3eac1b4`. Original task base `fba9ea50f` and unchanged intervening toolbar/context-menu history are controller-provided facts; no git commands were run.
- Read the diff in one review pass. The initial tool response truncated its middle; bounded reads recovered the omitted spans. Targeted line-number extraction came from that same package.
- Named external-source risk: whether `synthetic_accessibility_rect` reaches platform-exported node bounds. Checked the Widget hook contract, AccessNode bounds API, host dispatch (`ui/src/app.rs:5660`), and synthetic export (`ui/src/accesskit_bridge.rs:875`, `ui/src/accesskit_bridge.rs:961`). Confirmed the hook is not used by synthetic tree export.
- Named external-source risk: whether stale release suppression actually captures sibling input. Checked `ui/src/widget/row.rs:284` through `ui/src/widget/row.rs:326`; its capture short-circuit routes exclusively to the capturing child. Read only the ContextMenu selection/event function portions omitted by diff context (`ui/src/widget/context_menu.rs:328`, `ui/src/widget/context_menu.rs:590`) to establish which mouse events close/select and clear state.
- No test suites or focused tests were rerun: these findings follow directly from event branches and bridge contracts, and the existing reported runs do not cover them. No production files, index, commits, or branches were changed; only this requested review report was written.
- Reported existing evidence retained: Toolbar 15 passed/1 manual fixture ignored; ContextMenu widget 13 passed; broad context-menu filter 15 passed; layout 7 passed; legacy integration 1 passed; manual GPU fixture 1 passed; library check, formatting, and diff whitespace checks passed. These are implementer-reported results, not newly executed reviewer checks.
