# Release pipeline

Tilo releases six independent component families. A release tag for one family
does not fan out into unrelated builds:

| Component | Publication trigger | Expensive work | Published output |
|---|---|---|---|
| Server, Lounge, web entry, `tilo` command | A `vX.Y.Z` tag (t's `tag:vX.Y.Z` checks), or a dispatch of one of them on that tag | The frozen RPM set of each architecture that has a build node, the images, the musl builds of the command | Images under `sha-<commit>`, the version and `latest`; the CLI archives; the published `set-<fingerprint>-<arch>` RPM snapshots |
| Apps | A dispatch of t's app release checks (`apps-build-{arch}`, `sign-apps-{arch}`, `app-reproducible-{arch}`, `app-release-smoke-{arch}`) | Both native architectures: the compile, the signed package, and the render/engine smoke | Signed, reproducible tapps and immutable collection bundles built on t; registry publication is a separate step |
| Desktop session | A dispatch of `desktop-package-{arch}`; `publish-desktop-payload` publishes the OS payload | Both native architectures' host-package jobs, run serially per architecture | Install-tested Debian, RPM, Arch and Nix packages; the immutable OS payload under `publish-desktop-payload` |
| OS | `tools/ci/publish/os-release.sh` dispatched on a `vX.Y.Z` tag | Release image, sealed-image boot, boot signing, independent rebuild | Signed generation image, SBOM, provenance and freshness |
| Server OS | A dispatch on a `vX.Y.Z` tag (t's `publish-host`) | The server's programs, the package-snapshot base, the sealed image booted in a VM under Secure Boot | The signed server image, its UKI signed for Secure Boot, in `registry.tilo.so/tilo/tilo-host` under `sha-<commit>`, `gen<N>`, `<version>` and `latest` |
| Tilo Installer | Relevant protected `main` pushes; `imager-vX.Y.Z` for releases | Six native host-package jobs; signed installer media only on tags/manual rehearsals | Signed Linux/Windows and signed/notarized macOS downloads; immutable source-revision prereleases on main |
| Python and TypeScript SDKs | A `vX.Y.Z` tag (t's `publish-sdks`, `on = ["tag:v…"]`) | The two packages built and read back, and their suites, which the publish needs | `tilo-sdk` on PyPI and `tilo-sdk` on npm, at the tag's version; re-running the tag's step skips what is already published |

The SDKs are the one family with no build artifact of their own: the tag's version is written
into a copy of each manifest at publish time, and `main` builds the same packages with a
stand-in version and no credentials. Their native halves (platform wheels of `tilo-sdk`, and
the `tilo-sdk-native` packages) are designed in
[Publishing the SDKs](publishing.md#native-packages) and publish as a step beside this one when
those crates land. The two registry tokens are stored secrets, not identity tokens: neither
PyPI nor npm's provenance accepts Forgejo's identity.

Continuous app packaging is a dispatch of t's app release checks: `apps-build-{arch}`
compiles the default collection, `sign-apps-{arch}` signs it, `app-reproducible-{arch}`
proves two builds of an app match, and `app-release-smoke-{arch}` proves every app
draws and loads its engine. The packaging coordinator that would have dispatched
qualification runs was never enabled and is gone. OS releases happen only when
someone runs one on a release tag; never per push.

Kernel and kernel buildroot snapshots remain input-promotion work: they run only
when their pinned input set changes, and an OS release consumes their immutable
digests. The RPM snapshot is not: a release tag builds, signs and publishes the
package set its own tree resolves (t's `publish-rpms`), and the images of that tag
install from the published set, so nothing signs an image built from a set that is
not published. Main builds and tests the set and publishes nothing.

Each architecture's set is published on its own, so a release tag with only one
architecture's node connected publishes that architecture: its images install from the
set it published, and the other architecture's set is published by a dispatch on a node
of it. An image never installs from another architecture's set, and never signs one that
is not published.

## Fast validation versus release checks

Pull requests use the cheapest job that proves the affected boundary:

- Rust tests use the affected-package graph in **Check affected workspace packages**.
- The app release checks (`apps-build-{arch}` and its dependents) are dispatches,
  not part of an ordinary change's run.
- Desktop changes run the preflight contracts. Full distribution builds happen
  when `desktop-package-{arch}` is dispatched; the job builds cold, because a
  payload that can be published never compiles through a shared cache. The Arch
  and Nix packages build on their own too, in `package-arch` and `package-nix`,
  each in an image of its distribution.
- Full OS builds (`tools/ci/publish/os-image.sh`) do not run on ordinary
  `main` pushes. Every OS image carries the Browser's Chromium engine
  runtime, built from the pinned CEF distribution in the image build.
- Installer changes retain affected-package checks. Relevant protected `main`
  pushes also publish signed native Installer prereleases on all six existing
  platform/architecture jobs, subject to the exact-source qualification checks.
  Main reuses reviewed, signed published media; it does not build media or OS
  images. Stable tags retain distribution and website promotion. Manual runs
  default to Linux x86_64; tags, automatic main publication or `full_native`
  enable both architectures on Linux, Windows and macOS. See
  [Installer release operations](imager-release-operations.md#automatic-main-downloads)
  for required catalogs, native hosts and narrowly scoped broker authorization.
  The Forgejo repository/branch API supplies workflow protection preflight;
  it cannot replace the signing broker's authenticated protected-ref claims.

t keys every check by what it reads and serves a result across commits that keep
its inputs, so the app release needs no artifact hand-off between jobs:
`sign-apps-{arch}` reads `apps-build-{arch}`'s tree and `app-models`, and
`app-release-smoke-{arch}` reads both. A collection build packages the whole
default collection; t compiles only what changed. OS images and desktop
distributions can pin a published collection by version and architecture
without compiling apps again.

Both native architectures run on t: `desktop-package-amd64` and
`desktop-package-arm64` build and install-test every distribution userspace when
dispatched. This is not a support claim:
[the install contract](install-contract.json) still marks ARM64 desktop sessions
as planned, and architecture-specific install/release receipts and the product
owner's support-matrix decision remain separate from building them.

The Fedora desktop job also emits a small deterministic OS payload containing
the same install-tested `tilo-session`, `tilo-launcher`, `tilo-runtime`
and `tilo-unlock-helper` binaries. `publish-desktop-payload` publishes it
immutably and advances `TILO_DESKTOP_PAYLOAD_{URL,SHA256}_{X86_64,AARCH64}`.
Trusted OS composition downloads the matching architecture and omits those four
Cargo packages from its build. OS-only changes therefore test the desktop/OS integration without
recompiling the desktop. Scheduled/manual OS builds exercise this immutable
integration boundary; ordinary PRs do not run the OS image check. The OS
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
A publication advances the URL and digest variables. Architecture-specific
`TILO_APP_COLLECTION_{URL,SHA256}_{X86_64,AARCH64}` variables are maintained as
both native jobs publish; the unsuffixed pair remains the x86_64
compatibility channel.

An app publication requires `TILO_PACKAGE_PUBLIC_BASE_URL`, set to the public
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

## App selection and collections

Collections live in `dist/apps/app-collections.toml`. `default`, `desktop`, and
`os` intentionally resolve `*` from checked-in `apps/*/tilo-app.toml` files, so a
new app joins the normal fleet automatically. A tilolang package under `apps/tl`
is named beside it, and joins in the change that retires the native app it
replaces (`tl-notes`), so a release never ships two apps for one thing. Small
scenario-specific sets can extend another collection and exclude entries, or list
apps directly.

`image` is the set an OS image bakes into its read-only store: the `default`
collection, so every native app and the tilolang packages in place of those they
replaced. Each named package is compiled and signed by `tilolang build` and
installed through the same `tilo-install` a native tapp goes through; it runs on
the tilolang runtime bundle (one signed `tilolang/<version>` bundle directory),
which the image bakes into `/usr/lib/tilo-runtime` beside the Tilo runtime.

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

A dispatch of the app release checks builds the full default collection; t
compiles only what changed. Signing uses the configured `TAPP_PUBLISHER_IDENTITY`
secret. Manual imager runs with `full_native` rehearse the enabled Linux, Windows
and macOS matrix without publication; only the imager option allocates macOS
hosts.

App collection or desktop payload publication does not release an OS version.

## Building the OS on one machine

`tools/ci/build-os.sh` builds a commit's OS on one x86_64 or aarch64 machine with podman, such
as a remote build machine: the OS image (`image`, every layer of os/image/layers.toml), the
installer medium (`medium`), the development image (`dev-image`) and a board's sealed image
(`board <name>`), each after what it needs, with the stores, build directories and compiler
cache kept in the directory it is given. It is the only definition of these steps:
`tools/ci/signin-e2e-builder.sh` takes the image and medium from it.

## OS releases

An OS release is run on a `vMAJOR.MINOR.PATCH` tag, one step of
tools/ci/publish/os-release.sh at a time:

- The release image (os/image/build-layer.sh with `TILO_OS_FLAVOUR=release`) is the
  development image's layers (os/image/layers.toml) with those marked `release` built again
  from a thin-LTO compile and with the registry owner's key (os/publication.env).
- `seal` checks the image inside, installs it, which seals its UKI with
  the composefs digest the install computes, and boots it under KVM to the greeter.
- `sign-boot` signs every UKI with the EFI key and the target's PCR 11
  prediction for its generation with the PCR authority, both in KMS
  (tools/signing/boot-signer.sh), over what the seal left: no code of the image runs while
  it holds the keys. `apply` verifies those signatures against the pinned
  keys, adds them under `/boot`, installs the image once more to prove the seal holds,
  and writes its SPDX SBOM and SLSA provenance.
- Before the seal, `sign-os-loader-{arch}` (`sign-boot-loader.sh`, and
  `sign-os-loader-lenovo-83ed` for a board) signs the image's systemd-boot with the same
  EFI key and adds its `.efi.signed` companion, which `bootctl install` copies to the ESP;
  the signature has to be inside the image the seal describes, so it cannot be added to an
  installed disk afterwards.
- `reproduce` builds the release image again in one step, every action
  from a compile without caches, and must get the same
  digest; a difference fails it with a report of where the images differ.
- `sign` signs both images and their index (`index`) with the release
  key in KMS, and attests their provenance and a freshness statement valid for a day,
  each with a transparency-log entry. `publish` publishes each image as
  `gen<N>-<arch>`, the index as `gen<N>`, on a final version tag as `<version>`, and last
  `latest`; a `gen<N>` already published is never moved.

The generation is the next one of the signed published series
(os/dev/next-generation.sh), and the image carries it. A series starts with
`gen1`: `next-generation.sh` reads an absent repository as empty only under
`TILO_ALLOW_EMPTY_SERIES=1`, and only when the registry authenticated the read
and answered "name unknown". A genuinely absent repository, a deleted or
renamed one, and an unreadable one are otherwise the same answer, so the
first publication is published from a tree whose generation was allocated
that way on purpose with no signed series to read. `os-release.sh rehearse`
runs the boot signing, signing and publishing of a sealed image with throwaway
keys into an OCI layout, without credentials.

## Server OS releases

The server OS (os/Containerfile.host) releases through one check, on a release tag or when
someone dispatches it there:

```sh
t dispatch publish-host --ref tag:vX.Y.Z --publish
```

It builds what it needs and refuses to publish anything else: the server's programs
(`host-binaries`), their base (`host-base`: the mirrored bootc base, the frozen package set,
Firecracker and the tools that seal a UKI) and the image above it (`image-host`, carrying the
generation `generation-host` reads from the server's series), its boot loader signed into it
with the release EFI key (`sign-host-loader`), its UKI sealed with the composefs
digest its install computes (`seal-host`); it proves the image in a container (`test-image-host`) and by installing and
booting it under Secure Boot in a VM (`host-assurance-x86_64`); `sign-host-boot` signs its UKIs
and PCR 11 prediction with the EFI and PCR keys in KMS and `apply-host-boot` adds them, verified;
and `sign-host` signs it with the release key in KMS, with a transparency-log entry, and attests
its provenance. `publish-host` pushes it to `registry.tilo.so/tilo/tilo-host` as `sha-<commit>`,
as `gen<N>` once, as `<version>`, and as `latest` only for a final version tag; a published tag
is never moved. An image built from an RPM set no
release published is refused by signing, so a release installs from a published package set.
The image's container policy admits that repository, verified with the release key
(`tools/signing/release_trust.py`), so a server installed with it as its update origin
(`bootc install --target-imgref registry.tilo.so/tilo/tilo-host:latest`) updates from it.

`t dispatch host-release-rehearsal --ref BRANCH` rehearses the boot signing, signing and publishing
path with throwaway keys into a local OCI layout, verifying the published image as its consumers do, with no
credentials. It proves the release steps; it publishes nothing.

## The server's iteration loop

The server image is split so that a change to one program does not rebuild what the frozen
package set decides. `host-base` installs the packages and Firecracker and changes only when the
snapshot, the base or the package list does; t serves it across commits that keep them.
`image-host` copies the built programs and the files that name them onto it, so a change to one
program costs that compile and one copying layer, not the package set again. Both are dated by
the newest commit that changed their own files
(`tools/ci/publish/source-date.sh`), so a commit that changes neither is answered from t's cache
rather than rebuilt.

The loop for a change to a server program, a libexec script or a unit file:

```sh
t test test-image-host host-assurance-x86_64
```

`host-binaries` relinks the changed programs (the builder's compiler cache keeps a one-crate
change at a relink), `image-host` lays them on the cached base, `test-image-host` checks the
image in a container, `seal-host` seals it, and `host-assurance-x86_64` signs it with throwaway
keys, installs it and boots it under Secure Boot, proving the verified boot, the services, the
health verdict and the rollback. Nothing is signed with a release key or published on this
path. A local machine builds the same two layers with `os/build-host.sh`.

The development server image a dev VM runs as a server is that path's own, and never waits for
the release image: `image-host-dev` builds the server image's stages on the cached `host-base`
and then the additions of `os/Containerfile.host-dev` (the runtime that runs the device's
steps, the dev-VM CLI, their units, base.json). Its programs come from `host-binaries-dev`, which compiles the server image's
`os/components.toml` components and the dev additions without LTO, as the development OS is
compiled, so the compiler cache keeps a one-crate change to that crate's compile and the links
instead of the release image's whole-program ones. `test-image-host-dev` proves the sealed image
in a container, and `server-installer-medium-dev-x86_64` builds the medium that installs it.

## Dev VM disks

`tools/ci/publish/os-dev-publish.sh image` publishes main's dev image for one architecture,
the dev channel, once `tools/ci/publish/os-dev-check.sh` passed on it: the dev image (os/Containerfile.dev) on the
OS image of the same commit. The check is what a dev device needs, checked on the path a device
takes: the dev disk published last boots, stages the image with bootc and applies it live,
restarts into it to a greeter that takes input with its boot assessment passed, and takes the
next update live. A device falls back by itself from an image that does not boot, so the heavier
`tools/ci/publish/os-acceptance.sh` (devices, the desktop's journeys, the splash) checks a
release without holding back the dev channel. It boots overlays of the disk
`tools/ci/publish/os-dev-disk.sh` installed (a 64 GiB sparse disk, `os/dev/build-image.sh`,
never booted itself), which `os-dev-publish.sh disk` publishes once it passed. The OS image goes to
`registry.tilo.so/tilo/tilo-ci:main-<sha7>-<arch>`, a development-only repository that
production policy (`os/etc/containers/policy.json`) rejects, which
`tools/policy/check_dev_image_contract.py` keeps so. Dev images are never
signed with the OS release authority: each is signed with the development key
(`keys/cosign-dev.pub`, main only), which `os-dev-publish.sh` attaches.

`os/dev/publish-dev-disk.sh` then writes, for one architecture:

- `registry.tilo.so/tilo/tilo-vm:<tag>-<arch>`: the dev image child, signed, pushed by
  `tools/ci/publish/os-dev-publish.sh`, which moves `tilo-vm:<arch>-ci`, the architecture's dev
  channel, forward to it. Installed disks follow that tag.
- `registry.tilo.so/tilo/tilo-vm:<tag>`: an OCI index of every child of `<tag>`
  published so far. It is rebuilt from the child tags, and each job repeats
  that until a fresh read agrees, so it grows from one architecture to both
  without either job waiting. `latest` follows the newest revision's index, only forward, for `main` and
  release-tag runs.
- `tilo-vm-disk/<tag>/tilo-vm-<tag>-<arch>.<format>` in the generic package
  registry, each with a `.sha256` sidecar. qcow2 uses zstd-compressed clusters,
  which UTM and libvirt read directly.
- `tilo-vm-disk/<tag>/tilo-vm-disk-<arch>.json`: the tag, architecture, source
  revision, image and base digests, the ARM64 direct-boot companion files, and
  one entry per disk with its format, URL, SHA-256, size and virtual size. It
  is written last and is the commit point for that architecture: an
  architecture is published once its file exists, and a job whose file exists
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
- Desktop payload publication (t's `publish-desktop-payload`, dispatched on
  `main`) writes the immutable payload with the CI account's package token
  (`secrets.REGISTRY_TOKEN`) and advances the `TILO_DESKTOP_PAYLOAD_*` channel
  variables with the repository-scoped `secrets.RELEASE_TOKEN`; it needs no OIDC.
- Keep remote actions pinned to full commit hashes.
- Use the v3 artifact actions (or Forgejo's patched v4), because upstream v4's
  GitHub Artifact API is not compatible with this instance.
- Use artifacts for jobs in one workflow and the generic package registry for
  outputs consumed by later workflow runs.

## Lenovo board successors

A Lenovo board release (`os-release.sh sign-board` and `publish-board`, run on a release tag; the board images `os-image.sh board` builds are development images) publishes a separate
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
