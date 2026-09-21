# Release pipeline

Tilo releases four independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Apps | Relevant `main` pushes, `app-<directory>-vX.Y.Z`, `apps-vX.Y.Z`, or a manual **Release / apps** run with `publish` | Affected apps on routine `main`; selected apps once per enabled native architecture for explicit releases | Independently versioned qpkgs and immutable collection bundles |
| Desktop session | `desktop-vX.Y.Z`; scheduled/manual runs from `main` publish the OS payload | Enabled native host-package lanes; compiler objects shared per architecture | Debian, RPM, Arch and Nix packages plus checksums; immutable OS payload |
| OS | `vX.Y.Z`, or a manual **Release / OS** run from `main` with `publish` | Bootc image, sealed-image reproducibility and VM gates | Signed generation image, SBOM and provenance |
| Imager | `imager-vX.Y.Z` | Native host packages and signed installer media | Linux, Windows, and signed/notarized macOS downloads |

Continuous app packaging runs on relevant `main` pushes. It selects affected
qpkgs, reuses unchanged members from the prior immutable collection, and advances
the verified x86-64 channel only after publication succeeds. The guarded
packaging coordinator remains inactive; its qualification path is separate. The
[product matrix](ci-product-matrix.md) distinguishes available definitions from
qualified targets, including Windows apps still awaiting release qualification.
OS publication remains deliberate through tags or explicit dispatches.

Kernel, base-image, RPM and WPE buildroot snapshots remain input-promotion
workflows. They run only when their pinned input set changes; an OS release
consumes their immutable digests.

Automatic RPM snapshots cancel an obsolete run when a newer snapshot input
lands. Explicit snapshot publication is non-cancellable, verifies that its
input paths stayed unchanged, and rebases its resulting pin across unrelated
`main` commits before promotion.
Use an explicit **Snapshot / RPM repository** dispatch when a sequence of
package-input commits repeatedly supersedes the automatic run; it retains the
same signed native build and freshness checks.

## Fast validation versus release gates

Pull requests use the cheapest lane that proves the affected boundary:

- Rust tests use the affected-package graph in **Check affected workspace packages**.
- **Release / apps** computes the affected-app plan on PRs without native
  release packaging.
- Desktop PRs with matching packaging inputs run preflight contracts. Full
  distribution builds run weekly, on tags, or by explicit dispatch. Their
  sccache namespace is shared by architecture; compiler identity and native
  headers remain part of each cache key.
- Full OS builds run weekly, on tags, or by explicit dispatch, not on PRs or
  ordinary `main` pushes. WPE has its own signed snapshot gate; tagged OS
  releases require and include it.
- Ordinary imager changes use the affected-package checks. Native imager
  packaging runs only on tags or manual dispatches. Manual runs default to
  Linux x86_64; tags or `full_native` add Linux ARM64 and Windows on both
  architectures, including native Intel and Apple-silicon macOS packaging.

Release jobs never use an Actions cache as an artifact hand-off. Forgejo cache
entries are opportunistic and workflow artifacts are scoped to one run. Each
app qpkg is therefore published independently in Forgejo's generic package
registry. A collection build downloads unchanged qpkgs from the previously
pinned collection, replaces only the apps selected by the dependency/change
plan, and publishes a new immutable `.tar.gz`. OS images and desktop
distributions can pin one by version and architecture without compiling apps
again.

The desktop release retains its complete ARM64 matrix and verifier behind the
`TILO_DESKTOP_ARM64_RELEASES` capability switch. It is currently `"1"`: tagged
and explicit full-native builds use native amd64 and ARM64 runners. Weekly and default manual builds use the amd64 lane;
PRs run preflight only. Desktop package publication requires a tag.
No package or artifact definition needs to change if scheduling is paused.

The Fedora desktop lane also emits a small deterministic OS payload containing
the same install-tested `shell`, `tilo-launcher`, and `tilo-unlock-helper`
binaries. A successful `main` build publishes it immutably and advances
`TILO_DESKTOP_PAYLOAD_{URL,SHA256}_{X86_64,AARCH64}`. Trusted OS composition
downloads the matching architecture and omits those three Cargo packages from
its build. OS-only changes therefore test the desktop/OS integration without
recompiling the desktop. Scheduled/manual OS builds exercise this immutable
integration boundary; ordinary PRs do not run the OS image workflow.

Each architecture collection also contains the signed `tilo-runtime` bundle
built in the same Cargo graph as its thin app binaries. The OS verifies and
installs that exact runtime; it never independently relinks
`libruntime_dylib.so`, whose Rust symbol hashes would otherwise make reused
qpkgs ABI-incompatible.

Set repository variables `TILO_APP_COLLECTION_URL` and
`TILO_APP_COLLECTION_SHA256`, and `TILO_APP_COLLECTION_PUBLISHER` to make that
pinned bundle the normal OS input, or
provide the equivalent **Release / OS** dispatch inputs for a one-off build.
The outer SHA-256 is checked before extraction, every qpkg/index checksum is
checked again, every qpkg must carry the expected publisher identity, and
tagged OS provenance records the collection digest. Leaving the variables empty
preserves the source-build fallback for bootstrap work.
Successful default-collection publications from `main` advance the URL and
digest variables automatically. Architecture-specific
`TILO_APP_COLLECTION_{URL,SHA256}_{X86_64,AARCH64}` variables are maintained as
both native lanes publish; the unsuffixed pair remains the x86_64
compatibility channel.

App publication requires `TILO_PACKAGE_PUBLIC_BASE_URL`, set to the public
HTTPS generic-package base (currently
`https://code.sandbox.example/api/packages/qos/generic`). Registry traffic may use
the runner's internal endpoint, but receipts and persisted collection-channel
URLs always derive from this public base.

Manual app packaging defaults to both native architectures; PRs only plan
which apps are affected. During bootstrap, `architectures=x86_64` (or `aarch64`) may publish one architecture
without waiting for an unavailable native runner; tagged releases always
enforce the complete `x86_64 aarch64` matrix.

## App selection and collections

Collections live in `dist/apps/app-collections.toml`. `default`, `desktop`, and
`os` intentionally resolve `*` from checked-in `apps/*/tilo-app.toml` files, so a
new app joins the normal fleet automatically. Small scenario-specific sets can
extend another collection and exclude entries, or list apps directly.

The browser is the explicit exception: it links the digest-pinned WPE engine,
so the default collection excludes it and the OS build packages that one app
against its promoted WPE buildroot. An OS update using a collection therefore
reuses the independent app fleet and rebuilds only this native integration app.

Examples:

```sh
# Validate the recovery set locally.
python3 tools/apps/app_release.py --collection recovery

# Package two apps in one Cargo invocation.
TILO_RELEASE_APPS='notes calculator' dist/apps/package-apps.sh
```

The staged coordinator uses the dependency graph to select directly changed
apps plus app binaries that transitively depend on a changed workspace crate.
Reusing unchanged collection members requires a previously verified baseline;
the first qualification therefore builds the full selected collection. This
path remains separate from enabled tag/manual publication. A collection tag
validates the complete requested set. Component tags are immutable publication
boundaries and require the configured `QPKG_PUBLISHER_IDENTITY` secret.

Manual imager runs with `full_native` rehearse the enabled Linux, Windows, and
macOS matrix without publication. Desktop `full_native` includes ARM64; it does
not create a tagged package release, but a manual desktop run from `main`
**does publish the OS payload**. Only the imager option allocates macOS hosts.

Scheduled and manual OS integration builds retain the independent rebuild,
installed-VM, desktop, app, and OS integration gates. Continuous OS publication
and desktop-to-OS publication chaining are disabled in the workflows; setting
`TILO_CONTINUOUS_OS_RELEASE` alone does not enable them. App collection or
desktop payload publication does not release an OS version.

A manual **Release / OS** dispatch is also build-only by default. Enable its
`publish` input only from `main` to allocate and promote a signed generation.
This runs the same image, reproducibility, and VM gates as a safe integration
rehearsal while the production signing key is unavailable.

Publishing allocates both `imager-linux-x86_64` and
`imager-linux-aarch64`. Each native lane independently rebuilds, reseals,
boots, signs, generates an SBOM, and attests its child manifest with the same
source revision and generation. Only after both descriptors agree does the
generic promotion job construct an OCI index containing exactly those two
digests, sign it, attest provenance which names both children, and move
the immutable `genN`, source, and main-commit tags to that index. Every durable
write is read back and an exact retry is accepted; a conflicting immutable tag
fails closed. Only then does `latest` move as the release commit point.
No architecture-specific child is promoted as an update channel. Non-publishing
scheduled builds and manual rehearsals retain the single generic x86-64 lane,
so absent native release capacity does not block ordinary integration feedback.

## Forgejo-specific rules

- Do not add `permissions:`. Forgejo ignores it; repository capabilities belong
  in Authorized Integrations. OIDC jobs use `enable-openid-connect: true`.
- App publication uses two narrowly restricted Forgejo Actions (Local)
  integrations: app release tags and manual dispatches from `main`. Their
  audiences are stored in `TILO_APP_RELEASE_TAG_AUDIENCE` and
  `TILO_APP_RELEASE_MAIN_AUDIENCE`; no long-lived registry token reaches the
  publication job. The main integration has package-write and repository-write
  scope so it can advance only the pinned collection channel after publication.
- Trusted OS builds read the private immutable app collection with a separate
  read-only Forgejo Actions (Local) integration. The same integration reads the
  immutable desktop payload. Store its audience in
  `TILO_OS_APP_COLLECTION_AUDIENCE`. Restrict it to **Release / OS** events;
  currently these are tags, schedules and manual dispatches.
- Desktop payload publication uses a package-write/repository-write integration
  restricted to `package-desktop.yml` scheduled/manual runs on `main`; store its audience in
  `TILO_DESKTOP_RELEASE_AUDIENCE`.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.
