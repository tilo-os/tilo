# Release pipeline

Tilo releases four independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Apps | Relevant `main` pushes, `app-<directory>-vX.Y.Z`, `apps-vX.Y.Z`, or a manual **Release / apps** run with `publish` | Affected apps on routine `main`; selected apps once per enabled native architecture for explicit releases | Independently versioned tapps and immutable collection bundles |
| Desktop session | `desktop-vX.Y.Z`; scheduled/manual runs from `main` publish the OS payload | Enabled native host-package lanes; compiler objects shared per architecture | Debian, RPM, Arch and Nix packages plus checksums; immutable OS payload |
| OS | A dispatch on `stable` or a `vX.Y.Z` tag (`t dispatch publish-os-release --ref stable`) | Release image, sealed-image boot, boot signing, independent rebuild | Signed generation image, SBOM, provenance and freshness |
| Tilo Installer | Relevant protected `main` pushes; `imager-vX.Y.Z` for stable releases | Six native host-package lanes; signed installer media only on tags/manual rehearsals | Signed Linux/Windows and signed/notarized macOS downloads; immutable source-revision prereleases on main |

Continuous app packaging runs on relevant `main` pushes. It selects affected
tapps, reuses unchanged members from the prior immutable collection, and advances
the verified x86-64 channel only after publication succeeds. The packaging
coordinator that would have dispatched qualification runs was never enabled and
is gone; release-apps.yml keeps its guarded, disabled qualification path.
[Publishing with t](publishing-on-t.md) lists what each publishing workflow still
needs, including the Windows qualification.
OS releases happen only when someone dispatches one on stable or a release tag;
never per push.

Kernel, base-image, RPM and kernel buildroot snapshots remain input-promotion
workflows. They run only when their pinned input set changes; an OS release
consumes their immutable digests.

Automatic RPM snapshots cancel an obsolete run when a newer snapshot input
lands. Explicit snapshot publication is non-cancellable, verifies that its
input paths stayed unchanged, and rebases its resulting pin across unrelated
`main` commits before promotion.
Use an explicit **Build / RPM snapshot** dispatch when a sequence of
package-input commits repeatedly supersedes the automatic run; it retains the
same signed native build and freshness checks.

## Fast validation versus release gates

Pull requests use the cheapest lane that proves the affected boundary:

- Rust tests use the affected-package graph in **Check affected workspace packages**.
- **Release / apps** does not run on PRs; it plans and packages on `main`.
- Desktop PRs with matching packaging inputs run preflight contracts. Full
  distribution builds run weekly, on tags, or by explicit dispatch. Their
  sccache namespace is shared by architecture; compiler identity and native
  headers remain part of each cache key.
- Full OS builds run on t when a check needs them (`os-image-<arch>`, keyed by
  what each layer reads), on release candidates and when dispatched, not on
  ordinary `main` pushes. Every OS image carries the Browser's Chromium engine
  runtime, built from the pinned CEF distribution in the image build.
- Installer changes retain affected-package checks. Relevant protected `main`
  pushes also publish signed native Installer prereleases on all six existing
  platform/architecture lanes, subject to the exact-source qualification gates.
  Main reuses reviewed, signed published media; it does not build media or OS
  images. Stable tags retain distribution and website promotion. Manual runs
  default to Linux x86_64; tags, automatic main publication or `full_native`
  enable both architectures on Linux, Windows and macOS. See
  [Installer release operations](imager-release-operations.md#automatic-main-downloads)
  for required catalogs, native hosts and narrowly scoped broker authorization.
  The Forgejo repository/branch API supplies workflow protection preflight;
  it cannot replace the signing broker's authenticated protected-ref claims.

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

This describes workflow selection, not accepted release support:
[the install contract](install-contract.json) still marks ARM64 desktop sessions as
planned. Architecture-specific install/release receipts and the product owner's
support-matrix decision remain
separate from enabling the lane.

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
pinned bundle the normal OS input.
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
`https://code.tilo.so/api/packages/tilo/generic`). Registry traffic may use
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

The Browser is an ordinary member of the fleet. Its engine is not: the
Chromium engine runtime is a signed runtime bundle the OS image builds from the
pinned CEF distribution and installs at `/usr/lib/tilo-runtime/<version>`, and
the Browser's manifest names it under `[package].optional-runtimes`.

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

App collection or desktop payload publication does not release an OS version.

## OS releases

An OS release is t's os-release chain (tools/ci/publish/os-release.sh, the `os` family of
tools/ci/products.toml), dispatched on `stable` or a `vMAJOR.MINOR.PATCH` tag:

```sh
t dispatch publish-os-release --ref stable
```

- `os-release-image-<arch>` is the release image: the development image's layers
  (os/image/layers.toml) with those marked `release` built again from a thin-LTO compile
  (`os-compile-release-<arch>`) and with the registry owner's key (os/publication.env).
- `os-release-seal-<arch>` checks the image inside, installs it, which seals its UKI with
  the composefs digest the install computes, and boots it under KVM to the greeter. It also
  runs on every release candidate (stable).
- `sign-os-boot-<arch>` signs every UKI with the EFI key and the target's PCR 11
  prediction for its generation with the PCR authority, both in KMS
  (tools/signing/boot-signer.sh), over what the seal left: no code of the image runs while
  it holds the keys. `os-release-<arch>` verifies those signatures against the pinned
  keys, adds them under `/boot`, installs the image once more to prove the seal holds,
  and writes its SPDX SBOM and SLSA provenance.
- `os-release-reproduce-<arch>` builds the release image again in one step, every action
  from a compile without caches (`os-release-recompile-<arch>`), and must get the same
  digest; a difference fails it with a report of where the images differ.
- `sign-os-release` signs both images and their index (`os-release`) with the release
  key in KMS, and attests their provenance and a freshness statement valid for a day,
  each with a transparency-log entry. `publish-os-release` publishes each image as
  `gen<N>-<arch>`, the index as `gen<N>`, on a final version tag as `<version>`, and last
  `latest`; a `gen<N>` already published is never moved.

The generation is the next one of the signed published series
(os/dev/next-generation.sh), and the image carries it. A series starts with
`gen1`: `next-generation.sh` reads an absent repository as empty only under
`TILO_ALLOW_EMPTY_SERIES=1`, and only when the registry authenticated the read
and answered "name unknown". A genuinely absent repository, a deleted or
renamed one, and an unreadable one are otherwise the same answer, so the
first publication is published from a tree whose generation was allocated
that way on purpose with no signed series to read. `t test
os-release-rehearsal-x86_64` runs the boot signing, signing and publishing of
this tree with throwaway keys into an OCI layout, without credentials.

## Dev VM disks

`publish-os-dev-<arch>` publishes main's dev image and its disk for one architecture, the dev
channel, on main's newest head whenever t has room (and when dispatched: `t dispatch
publish-os-dev-<arch> --ref main`) once `os-dev-gate-<arch>` passed on them: the dev image
(os/Containerfile.dev) on the OS image of the same commit, installed onto a 64 GiB
sparse disk with `os/dev/build-image.sh`, never booted itself (the gates boot overlays
of it). The gate is what a dev device needs: the image boots to a greeter that takes input with
`boot-complete.target` reached, and takes the next update live. A device falls back by itself
from an image that does not boot, so the heavier `os-acceptance-<arch>` (devices, the desktop's
journeys, the splash) gates stable releases and runs on main when t has room, without holding
back the dev channel. The OS image goes to `registry.tilo.so/tilo/tilo-ci:main-<sha7>-<arch>`, a
development-only repository that production policy (`os/etc/containers/policy.json`)
rejects, which `tools/policy/check_dev_image_contract.py` keeps so. Dev images are never
signed with the OS release authority: `sign-dev-os-<arch>` signs each with the development key
(`keys/cosign-dev.pub`, main only), which `publish-os-dev-<arch>` attaches.

`os/dev/publish-dev-disk.sh` then writes, for one architecture:

- `registry.tilo.so/tilo/tilo-vm:<tag>-<arch>`: the dev image child, signed, pushed by
  `tools/ci/publish/os-dev-publish.sh`, which moves `tilo-vm:<arch>-ci`, the architecture's dev
  channel, forward to it. Installed disks follow that tag.
- `registry.tilo.so/tilo/tilo-vm:<tag>`: an OCI index of every child of `<tag>`
  published so far. It is rebuilt from the child tags, and each lane repeats
  that until a fresh read agrees, so it grows from one architecture to both
  without either lane waiting. `latest` follows the newest revision's index, only forward, for `main` and
  release-tag runs.
- `tilo-vm-disk/<tag>/tilo-vm-<tag>-<arch>.<format>` in the generic package
  registry, each with a `.sha256` sidecar. qcow2 uses zstd-compressed clusters,
  which UTM and libvirt read directly.
- `tilo-vm-disk/<tag>/tilo-vm-disk-<arch>.json`: the tag, architecture, source
  revision, image and base digests, the ARM64 direct-boot companion files, and
  one entry per disk with its format, URL, SHA-256, size and virtual size. It
  is written last and is the commit point for that architecture: an
  architecture is published once its file exists, and a lane whose file exists
  has nothing left to do.

`<tag>` is `main-<sha7>-ci`.

To fetch a disk, use a Forgejo token with package read access:

```sh
base=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/$TAG
curl -fsSL -H "Authorization: token $FORGEJO_TOKEN" "$base/tilo-vm-disk-x86_64.json" -o tilo-vm-disk.json
url=$(jq -er '.disks[] | select(.format == "qcow2") | .url' tilo-vm-disk.json)
sha=$(jq -er '.disks[] | select(.format == "qcow2") | .sha256' tilo-vm-disk.json)
curl -fL --retry 3 -C - -H "Authorization: token $FORGEJO_TOKEN" "$url" -o tilo-vm.qcow2
echo "$sha  tilo-vm.qcow2" | sha256sum --check --strict
```

To find the current tag, read the `org.opencontainers.image.version` label of
`tilo-vm:latest`, for example
`skopeo inspect docker://registry.tilo.so/tilo/tilo-vm:latest | jq -r '.Labels["org.opencontainers.image.version"]'`.

## Forgejo-specific rules

- Do not add `permissions:`. Forgejo ignores it; repository capabilities belong
  in Authorized Integrations. OIDC jobs use `enable-openid-connect: true`.
- Container images live on `registry.tilo.so/tilo/<image>`. Anyone may pull
  them, so job containers and pulls carry no credentials. A job that pushes
  logs in the way GitHub workflows log in to ghcr: `docker/login-action` with
  `registry: registry.tilo.so`, `username: ${{ github.actor }}` and
  `password: ${{ secrets.GITHUB_TOKEN }}` (or `podman`/`skopeo login` with the
  same values). The registry's token service checks the job token against the
  forge. Board (Lenovo) images and firmware use `code.tilo.so`. An explicit
  `allow_public_lenovo_firmware` dispatch may publish them under the public
  `tilo` owner while the board release is being brought up; other runs still
  require a private owner.
- App publication uses two narrowly restricted Forgejo Actions (Local)
  integrations: app release tags and manual dispatches from `main`. Their
  audiences are stored in `TILO_APP_RELEASE_TAG_AUDIENCE` and
  `TILO_APP_RELEASE_MAIN_AUDIENCE`; no long-lived registry token reaches the
  publication job. The main integration has package-write and repository-write
  scope so it can advance only the pinned collection channel after publication.
- Desktop payload publication (`release-desktop.yml` scheduled/manual runs on
  `main`) writes the immutable payload with the CI account's package token
  (`secrets.REGISTRY_TOKEN`) and advances the `TILO_DESKTOP_PAYLOAD_*` channel
  variables with the repository-scoped `secrets.RELEASE_TOKEN`; it needs no OIDC.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.

## Lenovo board successors

A Lenovo board release (`publish-os-release-lenovo-83ed` on t, dispatched on stable or a
release tag; the board images `os-board-<board>` are development images) publishes a separate
signed dense board series. Its fixed repository is `code.tilo.so/tilo/tilo`: each release retains
both `lenovo-83ed-<source>`, the update origin its image names, and `lenovo-83ed-gen<N>`.
`lenovo-83ed-latest` is a registry discovery hint, never permission to install an image. A
consumer must verify the exact image signature, profile/architecture/source/generation-bound
SLSA provenance and repository/digest-bound release-freshness attestation, then complete the
existing owner image-trust and measured-boot checks.

The Lenovo images published before the series existed all carry generation 1. An empty series
starts from one of them, pinned by digest as `TILO_LENOVO_SERIES_SEED` in
`os/publication.env` and verified as this board's signed generation 1
(`os/dev/board-generation.sh`): the build allocates generation 2, and the publication tags the
seed as `lenovo-83ed-gen1` and `-latest` before it publishes. There is no fallback to tag order,
commit counts or timestamps. Missing or conflicting history refuses allocation.
Generic OS generation allocation and Orange Pi one-off publication are unchanged.

This publication prerequisite does not change an installed immutable board
origin, authorize a new owner endorsement, or retire retained development
cohorts. On-device successor selection and compatible cohort retirement must
be qualified before claiming the ordinary Updates button delivers this series.
