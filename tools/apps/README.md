# Application release tooling

This directory owns the application release pipeline: selecting affected apps,
assembling architecture-specific collections, and creating and verifying their
publication receipts.

The programs are executable entry points used by `dist/apps/` and
`.forgejo/workflows/release-apps.yml`. Shared continuous-integration machinery
belongs in `tools/ci/`; repository-wide policy checks remain at the `tools/`
root.

Browser's signed `optional-runtimes` pin is a publication dependency, not a
manual installation step. Normal trusted-main releases build and publish that
engine before publishing a collection that builds or reuses Browser. The package
entry verifier waits for its exact runtime and native architecture in the
owner-signed catalog; submitting a package for admission is not availability.
Nonpublishing qualification does not authorize catalog updates.

To repair an absent engine without rebuilding an OS or the app collection, use
the existing trusted-main release mode:

```sh
fgj workflow run release-apps.yml -R tilo/tilo --ref main \
  -f engine_runtime_only=true -f publish=true -f architectures=x86_64
```

The `browser-engine-runtime-x86_64` artifact contains the signed thin engine
package and its payload objects from that run's exact source. Devices and native
VM assembly acquire it through the normal registry path; neither may substitute
an older engine or relabel a bundle as the requested version.

`tools/app_packaging_*.py`, `tools/assemble_verified_apps.py` and
`tools/canonical_verifier_bundle.py` are a deliberate temporary root-level
exception: release-apps.yml fetches them by exact reviewed path and digest, so
move them only together with those pins. New application tooling must be added
here, not to that exception set.
