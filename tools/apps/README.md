# Application release tooling

This directory owns the application release pipeline: selecting affected apps,
assembling architecture-specific collections, and verifying the signed output.
The programs are executable entry points used by `dist/apps/`, the app release
checks in `t.toml`, and the dev flows. Shared continuous-integration machinery
belongs in `tools/ci/`; repository-wide policy checks remain at the `tools/`
root.

Browser's signed `optional-runtimes` pin is a packaging dependency, not a manual
installation step. `engine-build-{arch}` and `sign-engine-{arch}` build and seal
that engine; `apps-build-{arch}` and `sign-apps-{arch}` build and sign a
collection. t builds and signs the release but does not submit it to the
`apps.tilo.so` registry; publication is a separate step outside these checks.

`tools/canonical_verifier_bundle.py` is a deliberate temporary root-level
exception: `verifier-bundle-{arch}` packs it with the tools it verifies, so move
it only with that check. New application tooling must be added here, not to that
exception set.
