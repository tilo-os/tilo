# Release pipeline

qOS releases four independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Apps | `app-<directory>-vX.Y.Z`, `apps-vX.Y.Z`, or a manual **Release / apps** run with `publish` | Selected apps once per native architecture | Verified qpkgs grouped in immutable collection bundles |
| Desktop session | `desktop-vX.Y.Z` | Native host packages; compiler objects shared per architecture | Debian, RPM, Arch and Nix packages plus checksums |
| OS | `vX.Y.Z`, or a manual **Release / OS** run from `main` | Bootc image, sealed-image reproducibility and VM gates | Signed generation image, SBOM and provenance |
| Imager | `imager-vX.Y.Z` | Native host packages and signed installer media | Linux, Windows and macOS downloads |

Kernel, base-image, RPM and WPE buildroot snapshots remain input-promotion
workflows. They run only when their pinned input set changes; an OS release
consumes their immutable digests.

## Fast validation versus release gates

Pull requests use the cheapest lane that proves the affected boundary:

- Rust tests use the affected-package graph in **Check affected workspace packages**.
- Desktop packaging runs only when desktop/package inputs change. Its sccache
  namespace is shared by architecture; compiler identity and native headers are
  part of each cache key, so ABI-incompatible objects cannot collide.
- OS pull requests build without WPE WebKit. WPE has its own signed snapshot
  gate, while tagged OS releases require and include it.
- Imager pull requests build and launch the Linux x86_64 package. A tag (or a
  manual run with `full_native`) expands to Linux ARM64, Windows and both macOS
  architectures before publication.

Release jobs never use an Actions cache as an artifact hand-off. Forgejo cache
entries are opportunistic and workflow artifacts are scoped to one run. App
collections therefore publish immutable `.tar.gz` bundles in Forgejo's generic
package registry; OS images and desktop distributions can pin and fetch one by
version and architecture without compiling the apps again.

Set repository variables `QOS_APP_COLLECTION_URL` and
`QOS_APP_COLLECTION_SHA256`, and `QOS_APP_COLLECTION_PUBLISHER` to make that
pinned bundle the normal OS input, or
provide the equivalent **Release / OS** dispatch inputs for a one-off build.
The outer SHA-256 is checked before extraction, every qpkg/index checksum is
checked again, every qpkg must carry the expected publisher identity, and
tagged OS provenance records the collection digest. Leaving the variables empty
preserves the source-build fallback for bootstrap work.

## App selection and collections

Collections live in `release/app-collections.toml`. `default`, `desktop`, and
`os` intentionally resolve `*` from checked-in `apps/*/qos-app.toml` files, so a
new app joins the normal fleet automatically. Small scenario-specific sets can
extend another collection and exclude entries, or list apps directly.

The browser is the explicit exception: it links the digest-pinned WPE engine,
so the default collection excludes it and the OS build packages that one app
against its promoted WPE buildroot. An OS update using a collection therefore
reuses the independent app fleet and rebuilds only this native integration app.

Examples:

```sh
# Validate the recovery set locally.
python3 tools/app_release.py --collection recovery

# Package two apps in one Cargo invocation.
QOS_RELEASE_APPS='notes calculator' dist/apps/package-apps.sh
```

On a normal main push, **Release / apps** selects only directly changed apps.
Shared UI/runtime changes are already covered by affected-package CI and do not
allocate a fleet packaging run by themselves; a collection tag validates the
complete requested set. Component tags are immutable publication boundaries
and require the configured `QPKG_PUBLISHER_IDENTITY` secret.
Manual desktop and imager runs with `full_native` exercise their full build
matrices without publishing, which makes them safe release rehearsals.

## Forgejo-specific rules

- Do not add `permissions:`. Forgejo ignores it; repository capabilities belong
  in Authorized Integrations. OIDC jobs use `enable-openid-connect: true`.
- App publication uses two narrowly restricted Forgejo Actions (Local)
  integrations: app release tags and manual dispatches from `main`. Their
  audiences are stored in `QOS_APP_RELEASE_TAG_AUDIENCE` and
  `QOS_APP_RELEASE_MAIN_AUDIENCE`; no long-lived registry token reaches the
  publication job.
- Trusted OS builds read the private immutable app collection with a separate
  read-only Forgejo Actions (Local) integration. Store its audience in
  `QOS_OS_APP_COLLECTION_AUDIENCE`. Pull-request jobs never receive or use that
  package capability; main, scheduled, and manual builds test the integration.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.
