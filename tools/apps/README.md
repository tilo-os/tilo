# Application release tooling

This directory owns the application release pipeline: selecting affected apps,
assembling architecture-specific collections, and creating and verifying their
publication receipts.

The programs are executable entry points used by `dist/apps/` and
`.forgejo/workflows/release-apps.yml`. Shared continuous-integration machinery
belongs in `tools/ci/`; repository-wide policy checks remain at the `tools/`
root.
