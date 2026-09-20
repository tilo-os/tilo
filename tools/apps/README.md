# Application release tooling

This directory owns the application release pipeline: selecting affected apps,
assembling architecture-specific collections, and creating and verifying their
publication receipts.

The programs are executable entry points used by `dist/apps/` and
`.github/workflows/release-apps.yml`. Shared continuous-integration machinery
belongs in `tools/ci/`; repository-wide policy checks remain at the `tools/`
root.

The packaging-controller closure is a deliberate temporary root-level
exception: `tools/app_packaging_*.py`, `tools/accept_app_artifacts.py`,
`tools/assemble_verified_apps.py`, and `tools/canonical_verifier_bundle.py` are
installed outside candidate checkouts at exact reviewed paths. Those paths are
bound by `app_packaging_product.REQUIRED_PATHS` and retained qualification
evidence. Move them only together with the external controller bundle and a
regenerated qualification policy. New application tooling must be added here,
not to that exception set.
