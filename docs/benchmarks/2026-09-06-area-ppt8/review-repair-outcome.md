# Reviewed repair freeze

Terra froze the repaired tests before any compilation/runtime. Root verified
all 33 manifest inputs: only lib.rs and render.rs differ from initial freeze;
the formatted wgpu_engine.rs bytes are unchanged. Production WGSL and routing
are unchanged from the initial reviewed candidate.

The missing argument is supplied. Linear, radial and sweep gradients occupy
independently visible regions; a varied 2x2 image and localized blur remain
visible. The odd-edge test checks four-way exact bytes, untouched sparse
pixels, nonempty/varied output, and no bump overflow. Required new GPU tests
fail if no adapter is found. Pure parser coverage proves default opt-in policy
without process-environment mutation; explicit fixtures exercise Area full,
sparse, CPU-unavailable, MSAA sparse rejection, ordinary shared diagnostics and
quad-skip preemption. This does not claim an executed full-MSAA comparison.

The added native 3840x2160 shared-coarse fixture compares all 33,177,600 full
output bytes from P4/P8, with fractional geometry/gradient at large native X.
It records sparse passes too but does not read back their native outputs;
four-way sparse parity is covered by the 49x17 fixture. It is a lightweight
correctness scene, not the timed workload. Its nonempty/varied-output and
overflow assertions must pass. The fixture is marked ignored for broad runs,
so runtime-validation-plan-v2.json explicitly runs it with --exact --ignored.

These are source-review facts, not successful test results. All tests, old
generated-permutation preservation, release build and timed runs remain pending.
