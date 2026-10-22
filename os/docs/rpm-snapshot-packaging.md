# Frozen native RPM snapshots

`os/Containerfile.rpms` resolves all architecture-derived direct package roots
against the pinned bootc base and includes their full weak-dependency closure
plus the Fedora kernel source RPM. The artifact is a scratch OCI image containing
`/repo`. It is a base-relative transaction, not a complete guest filesystem.

The stages preserve a strict boundary:

1. `configured-base` changes repository configuration only.
2. `metadata-tools` installs `createrepo_c` separately; `vendor-repo` verifies the
   pinned unsigned VirtIO RPM and exposes it as a local repository.
3. `resolve` inherits the original base package inventory. It downloads the
   complete transaction without installing tools or omitting weak dependencies.
4. `repository` authenticates each Fedora RPM, records hashes and signatures,
   and generates repository metadata. Its trust key is copied from the original
   immutable base, before any metadata-tool package installation.
5. `offline-check` starts again from the exact original `BASE_REF` and resolves
   and downloads every direct root with networking disabled and weak dependencies
   enabled. The final artifact depends on this stage so the check cannot be pruned.

Installing repository tools before resolution would hide those tools and their
dependencies from a base-relative snapshot. Checking closure in that same altered
environment would conceal the omission. The boundary regression tests cover both.

## Local preparation

Publication runs through t's RPM snapshot chain (`image-rpms-<arch>`,
`image-rpms`, `provenance-rpms`, `sign-rpms`, `publish-rpms`), which tags
`registry.tilo.so/tilo/rpms:set-<fingerprint>` from stable. Local builds are supported for preparation and
diagnosis; they do not publish, sign or promote an artifact. There is no
`os/dev/snapshot-rpms.sh` helper.

On the native target architecture, use the exact base pin and a named context
containing `virtio-win-0.1.302-1.noarch.rpm`. The recipe independently checks its
manifest SHA-256; an existing verified external-assets cache is suitable.

```sh
. os/base-snapshot.env
PACKAGES="$(python3 os/dev/derive-rpm-package-set.py --arch "$(uname -m)" | tr '\n' ' ')"
podman build --pull=never -f os/Containerfile.rpms \
  --build-context "github-assets=$VIRTIO_ASSETS_DIR" \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "PACKAGES=$PACKAGES" \
  -t "localhost/tilo-rpms:$(uname -m)-candidate" .
podman image inspect "localhost/tilo-rpms:$(uname -m)-candidate"
```

The pinned base must already be present locally when using `--pull=never`.
Snapshot creation downloads Fedora inputs; consumption uses the frozen repository.
Record the resulting immutable local repository digest before supplying
`RPMS_REF` to `os/dev/build-image.sh` (or let `resolve-rpm-snapshot.sh` look
up the published `set-<fingerprint>` tag). A local digest is not a
published fingerprint tag.

Every non-VirtIO RPM must produce a verified cryptographic signature and valid
digests using an isolated Fedora trust database. `rpmkeys` success alone is
insufficient: unsigned digest-only RPMs can also return success. The one permitted
unsigned RPM must match the reviewed VirtIO SHA-256 exactly.

The final artifact retains direct specs, individual RPM hashes, signature results
and the metadata-tool package inventory. Keep build/source metadata and logs with
local preparation evidence, and monitor physical host disk capacity during both
downloads and image layer creation. Compressed RPM totals do not include temporary
container caches, OCI storage layers or other OS payload artifacts.

## Local validation evidence (2026-09-16)

The ARM preparation artifact was assembled through equivalent disposable stages:
an untouched pinned-base download-only resolver, separate metadata tools, isolated
Fedora44 signature verification, and a fresh pinned-base offline closure check.
It contains 185 direct roots, 1,150 binary RPMs and the matching
`kernel-7.2.5-200.fc44.src.rpm`. Every binary NEVRA matched the measured transaction;
the resolver and offline check left their base package inventories unchanged.

The local artifact digest is
`localhost/tilo-rpms@sha256:3e8d4070f55b8e7975f0d9d236dab3969186c571707f825656f6bbe9b1f567ac`.
Its preparation records are retained on the preparation machine.
The stricter signature helper subsequently rechecked each Fedora RPM; signed,
wrong-key, tampered-payload and generated unsigned controls passed.

The final refactored Containerfile itself has not been rebuilt end to end locally:
physical disk headroom was reserved after artifact creation. Its stage boundaries,
signature rejection behavior, source paths and package-set invariants were tested.
The completed disposable-stage artifact is integration evidence for the underlying
transaction and authentication checks, not a claim that a fresh build of the
refactored recipe has already completed.
