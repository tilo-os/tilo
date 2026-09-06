# Release pipeline

qOS releases four independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Apps | A relevant `main` change, `app-<directory>-vX.Y.Z`, `apps-vX.Y.Z`, or a manual **Release / apps** run with `publish` | Changed apps once per enabled native architecture | Independently versioned qpkgs and immutable collection bundles |
| Desktop session | A relevant `main` change or `desktop-vX.Y.Z` | Enabled native host-package lanes; compiler objects shared per architecture | Debian, RPM, Arch and Nix packages plus checksums; immutable OS payload on `main` |
| OS | `vX.Y.Z`, or a manual **Release / OS** run from `main` with `publish` | Bootc image, sealed-image reproducibility and VM gates | Signed generation image, SBOM and provenance |
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
entries are opportunistic and workflow artifacts are scoped to one run. Each
app qpkg is therefore published independently in Forgejo's generic package
registry. A collection build downloads unchanged qpkgs from the previously
pinned collection, replaces only the apps selected by the dependency/change
plan, and publishes a new immutable `.tar.gz`. OS images and desktop
distributions can pin one by version and architecture without compiling apps
again.

The desktop release retains its complete ARM64 matrix and verifier behind the
`QOS_DESKTOP_ARM64_RELEASES` capability switch. It is currently `"0"` because
no native `runner-arm64` host is registered, so tags publish only x86-64
packages. Set it to `"1"` when that runner exists; no package or artifact
definition needs to be restored.

The Fedora desktop lane also emits a small deterministic OS payload containing
the same install-tested `shell`, `qos-launcher`, and `qos-unlock-helper`
binaries. A successful `main` build publishes it immutably and advances
`QOS_DESKTOP_PAYLOAD_{URL,SHA256}_{X86_64,AARCH64}`. Trusted OS composition
downloads the matching architecture and omits those three Cargo packages from
its build. OS-only changes therefore test the desktop/OS integration without
recompiling the desktop. Pull requests receive only a short-lived, workflow-
restricted package-read token, so they exercise the same immutable integration
boundary without a reusable registry credential.

Each architecture collection also contains the signed `qos-runtime` bundle
built in the same Cargo graph as its thin app binaries. The OS verifies and
installs that exact runtime; it never independently relinks
`libruntime_dylib.so`, whose Rust symbol hashes would otherwise make reused
qpkgs ABI-incompatible.

Set repository variables `QOS_APP_COLLECTION_URL` and
`QOS_APP_COLLECTION_SHA256`, and `QOS_APP_COLLECTION_PUBLISHER` to make that
pinned bundle the normal OS input, or
provide the equivalent **Release / OS** dispatch inputs for a one-off build.
The outer SHA-256 is checked before extraction, every qpkg/index checksum is
checked again, every qpkg must carry the expected publisher identity, and
tagged OS provenance records the collection digest. Leaving the variables empty
preserves the source-build fallback for bootstrap work.
Successful default-collection publications from `main` advance the URL and
digest variables automatically. Architecture-specific
`QOS_APP_COLLECTION_{URL,SHA256}_{X86_64,AARCH64}` variables are maintained as
native runners become available; the unsuffixed pair remains the x86_64
compatibility channel.

Manual app publication normally builds both native architectures. During
bootstrap, `architectures=x86_64` (or `aarch64`) may publish one architecture
without waiting for an unavailable native runner; tagged releases always
enforce the complete `x86_64 aarch64` matrix.

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

On a normal main push, **Release / apps** selects and publishes directly changed
apps plus app binaries that transitively depend on a changed workspace crate,
then assembles the default collection from those qpkgs and the unchanged
members of the prior collection. A collection tag validates the complete
requested set. Component tags are immutable publication boundaries and require
the configured `QPKG_PUBLISHER_IDENTITY` secret.
Manual desktop and imager runs with `full_native` exercise their full build
matrices without publishing, which makes them safe release rehearsals.

OS pushes remain non-publishing while CI capacity is constrained, but trusted
`main`, scheduled, and manual runs still perform the independent rebuild,
installed-VM, desktop, app, and OS integration gates. Once a
non-exportable `COSIGN_KEY_REF` is configured, setting
`QOS_CONTINUOUS_OS_RELEASE=1` turns relevant `main` pushes into the same
reproducibility, installed-VM, signing, SBOM, provenance, freshness, and
channel-promotion path used by explicit releases. It does not introduce a
weaker fast-publish path. A successful desktop payload publication requests
that OS integration run only when the switch is enabled; standalone app
updates continue to publish only their qpkg/collection boundary.

A manual **Release / OS** dispatch is also build-only by default. Enable its
`publish` input only from `main` to allocate and promote a signed generation.
This runs the same image, reproducibility, and VM gates as a safe integration
rehearsal while the production signing key is unavailable.

## Forgejo-specific rules

- Do not add `permissions:`. Forgejo ignores it; repository capabilities belong
  in Authorized Integrations. OIDC jobs use `enable-openid-connect: true`.
- App publication uses two narrowly restricted Forgejo Actions (Local)
  integrations: app release tags and pushes/manual dispatches from `main`. Their
  audiences are stored in `QOS_APP_RELEASE_TAG_AUDIENCE` and
  `QOS_APP_RELEASE_MAIN_AUDIENCE`; no long-lived registry token reaches the
  publication job. The main integration has package-write and repository-write
  scope so it can advance only the pinned collection channel after publication.
- Trusted OS builds read the private immutable app collection with a separate
  read-only Forgejo Actions (Local) integration. The same integration reads the
  immutable desktop payload. Store its audience in
  `QOS_OS_APP_COLLECTION_AUDIENCE`. Restrict it to **Release / OS** events;
  pull requests may use its short-lived package-read capability so their OS
  builds test the same pinned integration inputs as `main`.
- Desktop payload publication uses a package-write/repository-write integration
  restricted to `package-desktop.yml` pushes on `main`; store its audience in
  `QOS_DESKTOP_RELEASE_AUDIENCE`.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.
