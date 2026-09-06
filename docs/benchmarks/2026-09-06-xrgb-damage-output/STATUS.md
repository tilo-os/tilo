# XRGB damage-output candidate status

Goal remains active and unmet. No default promotion, commit or push.

The opt-in production path and ordinary benchmark counters are implemented.
Root reviewed both source freezes and requested missing error/cursor/history
coverage before runtime. The native library oracle compares 13 frames (initial
plus 12 real actions) at 3840x2160 through one and two targets against full
swizzles of each bridge's exact current staging image. It is separate from the
unchanged independent dense GPU oracle.

Version v1 validation started from 32 hashed source/configuration/fixture inputs.
Formatting and diff checks passed. Strict production Clippy failed with a
mutable-borrow compiler error in the scanout map-error branch and a rectangle
containment lint false positive. Both are preserved in the v1 log and summary;
all 32 hashes matched after the attempt. No runtime test, release build or
performance comparison ran for v1.

Version v2 added the unmodified toolchain pin to the manifest (33 inputs),
corrected the containment expression's lint shape, and added remaining cap/
damage tests. Its named mapping Result still retained the mutable borrow, so
strict compilation failed again. Source hashes remained unchanged.

Version v3 confines mapping, history, write, cursor and commit to a closure;
the compiler no longer reports borrow errors. Strict production lint now
reports six cleanup items: one unused binding, missing Errors documentation,
three long-function diagnostics and one branch-order style diagnostic. These
are preserved separately, with all 33 hashes matching after validation.
Terra subsequently completed that scoped cleanup and test-fixture lint scope.

The accumulator test now proves 64 distinct in-bounds pending rectangles remain
valid before the 65th invalidates the stale target; an earlier draft used an
out-of-bounds 65th rectangle and was corrected before runtime. The tracked
overflow test now executes the other target's next full paint, not just its plan.

Version v4 resolved the production-library issues; its example/test-inclusive
lint identified four reporting/test-fixture cleanup items. Strict vendor-test
lint passed. All failed v1-v4 checks remain recorded with immutable manifests
and complete logs.

Version v5 passes formatting/diff, strict production lint, strict vendor-test
lint and test-inclusive changed-line/compiler lint (zero relevant diagnostics).
All 17 planned validation commands pass: 803 passing test executions across
the selected suites and repeated configurations, zero failures, and seven
explicitly ignored older diagnostic tests. No adapter-unavailable skip occurred.
The original native GPU oracle passes D, B and BC this round. The new native
current-staging oracle passes X, BX and BCX with actual partial writes required
for both one and two targets. The 216-test DRM suite also passes D, BX and BCX.
All 33 frozen input hashes match after validation. The earlier independent
default GPU rounding mismatch is retained and is not proved fixed by one pass.

The ordinary release build passed in 9m 57.60s. Its build peak RSS was
10,763,012 KiB (compiler/build memory, not application RSS). Both executables
were copied to unused comparison paths and hashed; all 33 source hashes still
match. The UI executable is byte-identical to the prior canonical-order control.
No compiler/test process remains. Root authorises the fixed 24-attempt
exploratory comparison. Source stays frozen; no runtime overlaps editing.
These prerequisites do not establish performance or goal completion.

The comparison has now completed all 24 planned attempts, with 24 complete
1,000-action reports and no evidence mismatch. No shell run passed both gates;
one UI X control passed numerically, but X has no UI code path and is not an
attributed gain. Every shell run has 999 sparse + 1 full attempt and no recovery.

The matched bounds/XRGB comparison reduces median mean 25.130306→22.165605 ms
(-11.80%), but median p99 only 68.981→68.852 ms (-0.19%). CPU output falls
97.56%, while median peak process RSS changes +0.12%. Source and executable
hashes still match; all disk logs were re-read and their parsed fields match.
See RESULTS.md for the full matrix, every attempted run and limitations.

Goal remains active and unmet. After the completed read-only diagnostic audit,
root reopened only shell/examples/redraw_bench.rs for profile-only joined
per-action records in the next iteration, 2026-09-06-redraw-tail-correlation.
No further XRGB production changes are authorised. The v5 source was frozen
throughout its validation and comparison; its binaries, manifests and logs
remain preserved. Its source hash must not be treated as the new current
working-source hash once that separate diagnostic edit begins.

See comparison-contract.json and comparison-plan.json for the unchanged gates,
workload, variants, ordering and failure-preservation rules.
