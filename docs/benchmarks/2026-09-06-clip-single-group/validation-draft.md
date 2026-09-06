# Validation plan draft (not executable authorization)

Source editing is still open. No Cargo/format/runtime command may run until Terra freezes, root reviews the actual diff and required proofs, and a versioned source manifest/final validation plan is saved.

Prepared draft covers default vendor/shell/UI suites, original native retained/dense shell oracle with default/C/BC, native staging/history plus example/DRM suites with BXPC, and vendor/UI opt-in boundaries. C is clip-single-group, B is adaptive coarse + native path bounds, X is XRGB damage history, P is PPT8. Runtime flags are explicit; no default promotion. The final plan must additionally invoke each new required native/shared-input proof by its actual test name (including ignored proofs), and audit no-adapter behavior plus all guard returns/ignored tests separately from executed GPU coverage.

Four front-end checks will match prior policy: scoped rustfmt/diff check, strict production Clippy, strict vendor-tests Clippy, and test-inclusive changed-line/compiler diagnostics. The latter retains the established policy for unrelated test lint rather than hiding errors. Source and generated baseline checks follow; all 46 old generated shader token/metadata hashes plus the candidate's unchanged intra-workgroup suffix/workgroup metadata and expected reflected used-binding subset are checked (see reflection-clarification.md). This is source equivalence, not compiled-binary equivalence.

Stop and preserve any failed validation version. No patch while Cargo/runtime is running, and no retry of a failed version without a recorded repair/new freeze. Only a wholly passing reviewed version proceeds to ordinary release build and predeclared counterbalanced comparison.
