# Release pipeline

Tilo releases four independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Apps | Relevant `main` pushes, `app-<directory>-vX.Y.Z`, `apps-vX.Y.Z`, or a manual **Release / apps** run with `publish` | Affected apps on routine `main`; selected apps once per enabled native architecture for explicit releases | Independently versioned tapps and immutable collection bundles |
| Desktop session | `desktop-vX.Y.Z`; scheduled/manual runs from `main` publish the OS payload | Enabled native host-package lanes; compiler objects shared per architecture | Debian, RPM, Arch and Nix packages plus checksums; immutable OS payload |
| OS | `vX.Y.Z`, the nightly **Release / OS** run on `main` when `main` has moved, or a manual run from `main` with `publish` | Bootc image, sealed-image reproducibility and VM gates | Signed generation image, SBOM and provenance |
| Imager | `imager-vX.Y.Z` | Native host packages and signed installer media | Linux, Windows, and signed/notarized macOS downloads |

Continuous app packaging runs on relevant `main` pushes. It selects affected
tapps, reuses unchanged members from the prior immutable collection, and advances
the verified x86-64 channel only after publication succeeds. The guarded
packaging coordinator remains inactive; its qualification path is separate. The
[product matrix](ci-product-matrix.md) distinguishes available definitions from
qualified targets, including Windows apps still awaiting release qualification.
OS publication happens through tags, explicit dispatches, and one nightly run
on `main`; it never happens per push.

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
- Full OS builds run nightly, on tags, or by explicit dispatch, not on PRs or
  ordinary `main` pushes. The nightly run builds nothing when `main` is
  already published. WPE has its own signed snapshot gate; tagged OS
  releases require and include it.
- Ordinary imager changes use the affected-package checks. Native imager
  packaging runs only on tags or manual dispatches. Manual runs default to
  Linux x86_64; tags or `full_native` add Linux ARM64 and Windows on both
  architectures, including native Intel and Apple-silicon macOS packaging.

Release jobs never use an Actions cache as an artifact hand-off. Forgejo cache
entries are opportunistic and workflow artifacts are scoped to one run. Each
app tapp is therefore published independently in Forgejo's generic package
registry. A collection build downloads unchanged tapps from the previously
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
integration boundary; ordinary PRs do not run the OS image workflow. The OS
build reuses the payload only while it is current: the payload's
`manifest.json` records its source revision, and if any Rust or Cargo input
(`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo`, `crates`, `apps`,
`vendor`) changed between that revision and the build commit, or the revision
cannot be fetched, the OS build compiles the desktop from its own source
instead of shipping an older compositor.

Each architecture collection also contains the signed `tilo-runtime` bundle
built in the same Cargo graph as its thin app binaries. The OS verifies and
installs that exact runtime; it never independently relinks
`libruntime_dylib.so`, whose Rust symbol hashes would otherwise make reused
tapps ABI-incompatible.

Set repository variables `TILO_APP_COLLECTION_URL` and
`TILO_APP_COLLECTION_SHA256`, and `TILO_APP_COLLECTION_PUBLISHER` to make that
pinned bundle the normal OS input, or
provide the equivalent **Release / OS** dispatch inputs for a one-off build.
The outer SHA-256 is checked before extraction, every tapp/index checksum is
checked again, every tapp must carry the expected publisher identity, and
tagged OS provenance records the collection digest. Leaving the variables empty
preserves the source-build fallback for bootstrap work.
Successful default-collection publications from `main` advance the URL and
digest variables automatically. Architecture-specific
`TILO_APP_COLLECTION_{URL,SHA256}_{X86_64,AARCH64}` variables are maintained as
both native lanes publish; the unsuffixed pair remains the x86_64
compatibility channel.

App publication requires `TILO_PACKAGE_PUBLIC_BASE_URL`, set to the public
HTTPS generic-package base (currently
`https://code.podesta.ai/api/packages/qos/generic`). Registry traffic may use
the runner's internal endpoint, but receipts and persisted collection-channel
URLs always derive from this public base.

The signed registry entry is product-complete: `dist/apps/registry-metadata.toml`
defines each downloadable app's English name fallback, description, and card
tagline. `tools/apps/registry_metadata.py` detects shipped UI languages and
creates an owned icon for every app; checked-in screenshots are included when
available. The release uploads that immutable artwork under `apps.tilo.so`
before it submits the entry, so clients never receive a blank image set.

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
boundaries and require the configured `TAPP_PUBLISHER_IDENTITY` secret.

Manual imager runs with `full_native` rehearse the enabled Linux, Windows, and
macOS matrix without publication. Desktop `full_native` includes ARM64; it does
not create a tagged package release, but a manual desktop run from `main`
**does publish the OS payload**. Only the imager option allocates macOS hosts.

Scheduled and manual OS integration builds retain the independent rebuild,
installed-VM, desktop, app, and OS integration gates. Per-push OS publication
and desktop-to-OS publication chaining are disabled in the workflows; setting
`TILO_CONTINUOUS_OS_RELEASE` alone does not enable them. App collection or
desktop payload publication does not release an OS version.

## Nightly OS publication

Developers and agents build dev VMs from a published, signed OS image, so
`code.podesta.ai/qos/tilo:latest` must not depend on someone remembering to
dispatch CI. **Release / OS** therefore runs every night at 02:00 UTC on
`main` and publishes a signed generation through exactly the protected manual
`main` path: the same native `runner-disk-100g`/`runner-disk-100g-arm64`
lanes, KMS signing preflight, reproducibility, installed-VM and AT-SPI gates,
SBOM, provenance and freshness attestations, and the shared
`tilo-os-image-series` generation lock. It also receives the same protected
PCR-prediction and EFI signing inputs, and ships the full image including the
browser, like a default manual publication, once `os/wpe-buildroot-snapshot.env`
pins a promoted WPE buildroot for both architectures. Until then the nightly
image omits the browser, with a warning, instead of failing every night; `v*`
tags still require the browser.

Before allocating any build lane, the plan job reads the
`org.opencontainers.image.revision` label of the current `latest` image with
`skopeo inspect`. When it equals the scheduled commit, the run ends there:
nothing is built, signed or promoted, and no generation is consumed. Any
failure to read the label, including the first run when no image exists yet,
publishes instead, so registry trouble cannot leave the channel stale; the
cost of that fallback is one redundant release. Because the check runs inside
the generation lock, a nightly run queued behind a manual publication of the
same commit also skips.

The installed-desktop gate boots the candidate with a virgl GPU when the runner
exposes a usable `/dev/dri/renderD*`, and with a plain 2D virtio GPU otherwise;
the Kata-confined runner kernels have no DRM subsystem. Tilo's DRM backend
paints on the CPU into dumb buffers and the guest carries Mesa software
rendering, so the first-login OOBE, compositor, lock and AT-SPI gates run and
must pass in either mode. The display mode never selects which gates run.

Only runs that can publish (tags, manual `main` runs with `publish`, and
scheduled `main` runs) share the `tilo-os-image-series` concurrency group.
Non-publishing integration builds allocate no generation and serialize per
ref instead, so a long branch rehearsal never delays a release.

The nightly image carries `latest`, `genN` and `main-<sha7>` like a manual
`main` publication, so anything triggered by a published OS release follows
it without a separate schedule. Runner cost is bounded to at most one
two-architecture release per day and none on days without `main` changes.

## Dev VM disks and dev-trust integration images

Dev VMs must not wait on release gating. Every `main` build that gets past the
image build pushes that image, unsigned, as
`code.podesta.ai/qos/tilo-ci:main-<sha7>-<arch>`. `tilo-ci` is a
development-only repository: production policy
(`os/etc/containers/policy.json`) rejects it, and
`tools/policy/check_dev_image_contract.py` keeps it that way. The push never
blocks a release.

Publication refuses to start without its protected inputs
(`TILO_PCR_PREDICTION_SIGNER`, `TILO_EFI_SIGNING_CERT_SHA256` and
`TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY`). Until all three are configured, the
nightly run is a `main` integration build instead of a publication, with a
warning. It still runs every gate and produces the `tilo-ci` image and a dev
disk, and it skips when this commit's integration image already exists. Once
the inputs are configured, the nightly run publishes without further changes.

After the build, `dispatch-dev-disk` requests **Release / Dev VM disk** once
per run:

- when the run promoted a signed release, from its exact index digest, as
  `tilo-vm:main-<sha7>` (or the release version);
- otherwise, on `main`, from the run's x86-64 `tilo-ci` image after checking
  that its revision label names the run's commit, as `tilo-vm:main-<sha7>-ci`.

`dev-disk.yml` derives the base's trust from its repository. A `tilo` base must
carry the release signature and provenance naming the source revision; a
`tilo-ci` base must carry that revision as its OCI label and may publish only
`-ci` tags, so it can never take a name a signed release would use. The disk
manifest records `baseImage` and `baseTrust`. `tilo-vm vm create` takes the
newest `main-*` disk, so developers get the freshest image either way. Dispatch
`dev-disk.yml` with `base_image=code.podesta.ai/qos/tilo-ci:main-<sha7>-x86_64`
to rebuild a disk by hand.

A manual **Release / OS** dispatch is build-only by default. Enable its
`publish` input only from `main` to allocate and promote a signed generation
without waiting for the nightly run.
This runs the same image, reproducibility, and VM gates as a safe integration
rehearsal while the production signing key is unavailable.

A tag publication allocates both `imager-linux-x86_64` and
`imager-linux-aarch64`; a manual or nightly `main` publication uses
`runner-disk-100g` and `runner-disk-100g-arm64`. Each native lane independently rebuilds, reseals,
boots, signs, generates an SBOM, and attests its child manifest with the same
source revision and generation. Only after both descriptors agree does the
generic promotion job construct an OCI index containing exactly those two
digests, sign it, attest provenance which names both children, and move
the immutable `genN`, source, and main-commit tags to that index. Every durable
write is read back and an exact retry is accepted; a conflicting immutable tag
fails closed. Only then does `latest` move as the release commit point.
No architecture-specific child is promoted as an update channel. Non-publishing
manual rehearsals retain the single generic x86-64 lane,
so absent native release capacity does not block ordinary integration feedback.

A publication reads the next generation from the signed published series and
refuses to run when `code.podesta.ai/qos/tilo` does not exist, so a renamed
or deleted repository can never silently restart the series at `gen1`. The
very first publication therefore needs an explicit operator decision: set the
repository variable `TILO_OS_SERIES_GENESIS_REVISION` to the full 40-hex
`main` commit being published, then dispatch **Release / OS** with `publish`.
Only that exact commit may start an absent series, and only after the
registry authenticated the request and answered "name unknown". Once `gen1`
exists the repository exists too and the variable has no effect; delete it.

## Dev VM disks

A published OS generation requests **Release / Dev VM disk**
(`dev-disk.yml`) for its exact source revision and index digest. The workflow
can also be dispatched by hand for any commit whose production image is
published; it never runs on pushes or pull requests. Each native lane verifies
the base against `keys/cosign.pub` and requires the child's signed provenance to
name the same source revision. It then builds `os/Containerfile.dev` on that
base, installs and seals a 64 GiB sparse disk with `os/dev/build-image.sh`, and
boots the disk until SSH answers. Dev images are never signed with the OS
release authority.

The outputs are:

- `code.podesta.ai/qos/tilo-vm:<tag>`: an immutable OCI index. The installed
  disk updates from this tag. `latest` moves last, and only forward, for `main`
  and release-tag runs.
- `tilo-vm-disk/<tag>/tilo-vm-<tag>-<arch>.<format>` in the generic package
  registry, each with a `.sha256` sidecar. qcow2 uses zstd-compressed clusters,
  which UTM and libvirt read directly. VHDX (Hyper-V) is dynamic and opt-in
  through the `formats` input.
- `tilo-vm-disk/<tag>/tilo-vm-disk.json`: the tag, source revision, image and
  base digests, and one entry per disk with its arch, format, URL, SHA-256, size
  and virtual size. This manifest is written last and is the commit point for
  the disk package version.

`<tag>` is `main-<sha7>` for `main`, `<version>` for a release tag, and
`<branch>-<sha7>` for other dispatches. aarch64 disks are one `architectures`
entry away (`x86_64 aarch64`) and use `runner-disk-100g-arm64`.

To fetch a disk, use a Forgejo token with package read access:

```sh
base=https://code.podesta.ai/api/packages/qos/generic/tilo-vm-disk/$TAG
curl -fsSL -H "Authorization: token $FORGEJO_TOKEN" "$base/tilo-vm-disk.json" -o tilo-vm-disk.json
url=$(jq -er '.disks[] | select(.arch == "x86_64" and .format == "qcow2") | .url' tilo-vm-disk.json)
sha=$(jq -er '.disks[] | select(.arch == "x86_64" and .format == "qcow2") | .sha256' tilo-vm-disk.json)
curl -fL --retry 3 -C - -H "Authorization: token $FORGEJO_TOKEN" "$url" -o tilo-vm.qcow2
echo "$sha  tilo-vm.qcow2" | sha256sum --check --strict
```

To find the current tag, read the `org.opencontainers.image.version` label of
`tilo-vm:latest`, for example
`skopeo inspect docker://code.podesta.ai/qos/tilo-vm:latest | jq -r '.Labels["org.opencontainers.image.version"]'`.

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
- The release-signing role (`AWS_SIGNING_ROLE_ARN`, OIDC audience
  `tilo-release-signing`) must trust scheduled runs on `main` as well as
  manual `main` dispatches and `v*` tags; the nightly publication signs with it.
- Desktop payload publication (`package-desktop.yml` scheduled/manual runs on
  `main`) writes the immutable payload with the CI account's package token
  (`secrets.REGISTRY_TOKEN`) and advances the `TILO_DESKTOP_PAYLOAD_*` channel
  variables with the repository-scoped `secrets.RELEASE_TOKEN`; it needs no OIDC.
- Dev VM disks are development artifacts, never release-signed, so
  `dev-disk.yml` uploads them with the CI account's package token
  (`secrets.REGISTRY_TOKEN`, as `build-kernel.yml` uses) rather than an
  Authorized Integration, and needs no OIDC. Disk uploads go to
  `TILO_PACKAGE_PUBLIC_BASE_URL`, because the 100 GiB runners do not resolve the
  in-cluster Forgejo service.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.
