# SOLID command fusion audit (pre-implementation)

The native vendor owner reviewed the GPU command writer and fine interpreter.
Every `write_path` caller immediately emits one COLOR, gradient, IMAGE, BLUR,
or END_CLIP command. A zero-segment path currently emits a standalone SOLID
command, assigning all four per-thread area values to 1.0. A paired private
coarse/fine permutation can move precisely those assignments into its successor
and remove one PTCL word/interpreter iteration per such pair. END_CLIP must be
included: it consumes the area for clip coverage. Segmented FILL is unchanged.

Use an explicit boolean result from write_path, passed to the immediate paint
writer; avoid a pending private flag that can leak between commands. A reserved
high opcode bit identifies fused area setup. Never flag FILL, BEGIN_CLIP, JUMP
or END. If allocation inserts a JUMP between setup and paint, it does not touch
area or color state; moving setup to the reached paint remains equivalent.

The ordinary fine interpreter cannot consume these tags. Selection therefore
must pair the fused fine shader with the matching dense-grid, legacy, or active
coarse permutation. A missing partner keeps the original shader of the SAME
mode and grid. Active native tile bounds remain intact. Full, CPU, recovery and
shared-coarse diagnostic paths keep original streams. Quad-skip and opaque-prune
need explicit conservative precedence, not accidental mixed permutations.

No paint, clip state, ancestor background, antialiasing arithmetic or floating
operation is removed. Unlike opaque pruning there is no NaN-times-zero overwrite
assumption. Unlike a row mask there is no producer/storage rounding proof. No new
GPU buffers, bindings or copies are needed. Added decode/branch overhead may
offset fewer commands, so this is not a speedup claim.

Required tests: exact env parsing and requested/available/actual-selected flags;
mode-matched producer/consumer and unavailable-partner fallback recording matrix;
actual GPU PTCL flags with shortened lists, dynamic JUMPs and unchanged segmented
FILL; exact dense/original-sparse/fused pixels across all supported paint tags,
nested/deep clips including END_CLIP, partial native edges, disjoint masks and
overflow/recovery. Mask private tags in any diagnostic parser that reads them.
Do not widen pixel tolerances or simplify the benchmark workload.

Implementation is conditional on the follow-up telemetry confirming reachable
SOLID/successor adjacency. Acceptance still requires repeated unprofiled native
4K/1000-action measurements on an ordinary release build, then fresh unflagged
default runs. Command counts alone cannot satisfy the performance goal.
