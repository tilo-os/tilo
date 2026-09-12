# Transparent Pixel Comparison Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shared offscreen frame comparator compare visible straight-alpha output correctly on Metal and Vulkan without increasing its antialiasing tolerance.

**Architecture:** Ignore RGB only when both corresponding stored alpha bytes are exactly zero. Keep the existing four-channel comparison in every other case and keep unmatched-buffer accounting. This changes the shared test oracle, not rendering or presentation.

**Tech Stack:** Rust, Vello/wgpu, RGBA8 straight-alpha offscreen output, native Metal and Fedora software Vulkan.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, sections 7 and 11.

## Global Constraints

- Before timing, compare retained/full renders on each backend with documented pixel tolerance and controlled fonts.
- Keep Mac rendering expectations explicit about Metal, fonts, scale and pixel tolerance instead of weakening Linux goldens to make them pass on another backend.
- Preserve the existing ±1 comparison for every pixel where either alpha byte is nonzero. Do not change shaders, native damage, byte-preservation assertions, golden images or production timeout policy.
- The full port still requires a Mac shell with real embedded Notes/Files, a complete other-app capability map, Fedora headless and all required tests on both hosts. This prerequisite is not full-goal completion.
- Work in `/Users/xaver/dev/qos-macos-shell`, branch `codex/macos-shell`. Mac target is `/Users/xaver/dev/qos/target/macos-shell`; the controller handles Fedora sync and validation.

## Evidence

At `0063daf84`, native Metal diagnostics measured 12 sparse-prefix mismatches,
72 geometric-capacity mismatches and 47 CPU-tag-sequence mismatches. Every
classified pair had alpha zero on both sides; RGB differed. The last sequence
failed after CPU tags were disabled, so changing CPU-tag processing alone is
not supported by this evidence. The original full-suite failure locations were
not all reproduced and later assertions still require execution after this fix.

The comparator currently treats arbitrary straight RGB beneath stored alpha zero
as a visible discrepancy. Fine-shader output divides premultiplied RGB by bounded
alpha before RGBA8 storage; tiny coverage may preserve RGB while alpha rounds to
zero. The measured bytes justify correcting comparison semantics, without a
claim about a specific Metal compiler cause.

## Task 1: Compare transparent straight-alpha pixels by visible output

**Files:**
- Modify/test: `ui/src/offscreen.rs` (module documentation, `pixels_differing_beyond_aa`, existing test module).

**Interfaces:**
- Consumes tight-packed RGBA8 buffers in `OffscreenRenderer::pixels_differing_beyond_aa(a: &[u8], b: &[u8]) -> usize`.
- Produces the same public signature and unmatched-pixel counting. Fully transparent RGB alone no longer contributes a difference.

- [ ] Add this pure regression next to `frame_comparison_tolerates_one_step_and_no_more`, leaving that existing test intact:

```rust
#[test]
fn frame_comparison_ignores_rgb_only_when_both_pixels_are_fully_transparent() {
    for (actual, expected, differences) in [
        ([0, 0, 0, 0], [81, 209, 119, 0], 0),
        ([10, 20, 30, 1], [12, 20, 30, 1], 1),
        ([10, 20, 30, 0], [12, 20, 30, 1], 1),
        ([12, 20, 30, 1], [10, 20, 30, 0], 1),
        ([10, 20, 30, 0], [10, 20, 30, 2], 1),
        ([10, 20, 30, 254], [10, 20, 30, 252], 1),
    ] {
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
            differences,
            "actual={actual:?}, expected={expected:?}"
        );
    }

    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(
            &[0, 0, 0, 0, 81, 209, 119, 0],
            &[255, 0, 255, 0],
        ),
        1,
        "transparent pixels without a counterpart remain a size mismatch"
    );
}
```

- [ ] Run the new pure regression before changing the comparator. Command: `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo nextest run -p ui --lib --locked --profile ci -E 'test(frame_comparison_ignores_rgb_only_when_both_pixels_are_fully_transparent)'`. Expected failure: the first pair reports one discrepancy instead of zero.

- [ ] Replace only the existing overlapping-pixel filter with:

```rust
.filter(|(x, y)| {
    if x.get(3) == Some(&0) && y.get(3) == Some(&0) {
        return false;
    }
    x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 1)
})
```

  Keep `chunks_exact(4)`, the overlap count, `a.len().abs_diff(b.len()).div_ceil(4)` and saturating addition unchanged.

- [ ] Document the stored straight-alpha rule on the comparator and in the module's comparison guidance: fully transparent RGB is invisible; if either alpha is nonzero all channels keep their current ±1 tolerance. Keep exact untouched-pixel assertions unchanged because they prove byte preservation, a different property.

- [ ] Format `ui/src/offscreen.rs`, then run all offscreen tests serially on native Metal: `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell WGPU_BACKEND=metal cargo nextest run -p ui --lib --locked --profile ci --test-threads=1 -E 'test(offscreen::tests::)'`. This includes the pure comparator regressions and all three previously failing GPU cases, with later capacity/CPU-stage transitions now reached. If a remaining failure has visible alpha, report and diagnose it rather than broaden this rule.

- [ ] Run `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings`.

- [ ] Commit only `ui/src/offscreen.rs` as `fix(ui): ignore invisible RGB in offscreen frame comparisons`. Report exact native commands/results and all remaining failures. Do not claim the full UI suite passes from this scoped result.

- [ ] Controller fast-forwards Fedora when no earlier test is running and executes the same serial offscreen group at the exact commit, with `WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json`, private `XDG_RUNTIME_DIR` and `CARGO_TARGET_DIR=/home/parallels/qos-target`. Record the commit, test count and outcome.

## Scope and review

Review the shared oracle semantics, visible-alpha counterexamples, preserved
buffer-length behavior and unchanged damage/fixture assertions. Font catalog,
search pagination and unrelated app-theme failures remain separate required
work. App lifecycle, platform providers and production shell hosting are covered
by subsequent subsystem plans under the architectural specification.
