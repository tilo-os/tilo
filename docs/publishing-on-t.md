# Publishing with t

Tilo publishes container images, the OS, its apps and the imager. This document lists every
workflow in `.forgejo/workflows/` that publishes something, says what it needs, and how it moves
onto [t](https://code.tilo.so/tilo/t), which already runs Tilo's checks ([t.md](t.md)). t's side
of this is described in its [pipelines document](https://code.tilo.so/tilo/t/src/branch/main/docs/pipelines.md).

## The rules

- **Main publishes development artifacts only; releases come from `stable` and release tags.**
  Everyone pushes to `main`, so whatever `main` can publish, anyone can publish. Production
  images, OS releases and app collections are built from `stable` and release tags, which only
  the stabilizer pushes, and t publishes them by itself when those move.
- **Building and signing are separate checks.** A build runs arbitrary code (build scripts,
  dependencies), so it holds no secrets. The check that signs `needs` the build, receives its
  outputs, refuses any digest but the one the build produced, and is the only check that may
  sign. Pushing is a third check, which may push but not sign.
- **Publishing holds no stored credentials.** A publishing check declares `id_token =
  ["<audience>"]` and asks t, while it runs, for an OpenID Connect identity token (t's
  [identity tokens](https://code.tilo.so/tilo/t/src/branch/main/docs/pipelines.md#identity-tokens)).
  The token names this repository, the reference and the commit, is meant for one audience
  and lasts minutes, so there is nothing long-lived to leak: a registry token or access key is
  worth something to whoever takes it until someone notices and rotates it, a stolen identity
  token expires before that. The registry and AWS check its signature against t's published
  keys and decide from its claims what the job may do.
- **Signing keys never leave KMS.** The release keys (`alias/tilo-release-signing`,
  `alias/tilo-pcr-prediction`, `alias/tilo-efi-signing`) are non-exportable AWS KMS keys. A
  signing check exchanges its identity token for a role that may only ask KMS to sign with
  them, and only for `stable` and `v*` tags.
- **t decides who gets tokens and secrets**, from its server config, never from this
  repository: each audience (`[[oidc.audience]]`) and each `[[secret]]` names the references
  (`refs`) it may reach. A run of someone's working tree never gets any.
- **Credentials reach only a kernel of their own.** A check holding a secret or an identity
  token runs as a micro-VM step (t's README, "Micro-VM steps"): a guest kernel nothing else
  shares, on any node that runs them, so no machines are set aside for releases. The builds a
  release signs run in trusted runs only from trusted results, in fresh directories, never from
  a working tree's cache.

## How a workflow becomes checks

| Forgejo Actions | t |
|---|---|
| `on: push: branches: [main]` | `on = ["main"]` (and `stable`, or `follow = [...]` in the server config for others) |
| `on: push: tags: [v*]` | `on = ["tag:v*"]` |
| `on: schedule` | `on = ["schedule:<cron>"]`, UTC, on main's head |
| `workflow_dispatch` with inputs | `on = ["dispatch"]` with `[check.dispatch_inputs]`; `t dispatch <check> --ref stable --input k=v` |
| `pull_request` (build without publishing) | the build check itself: `t test image-<name>` before pushing |
| `needs:` plus `upload-artifact`/`download-artifact` | `needs = [...]` with the need's `outputs` |
| `enable-openid-connect` | `id_token = ["<audience>"]`: the same `ACTIONS_ID_TOKEN_REQUEST_*` variables, from t's issuer |
| `secrets.*` | `secrets = [...]`, from the server config, by reference and node |
| `runs-on: runner-arm64`, `runner-disk-100g` | `requires = ["arch=arm64"]`, `requires = ["disk-100g"]` |
| KVM, loop devices | `requires = ["/dev/kvm"]`, `devices = ["/dev/loop-control"]`, `privileged = true` |
| `macos-aarch64` runners | `requires = ["os=macos", "arch=arm64"]` (t's macOS workers) |
| Windows runners | none: see [Windows](#windows) |
| `if: github.ref == 'refs/heads/main'` and similar | the channel from `T_REF` (`tools/ci/publish/lib.sh`), or one check per channel |
| `${{ github.sha }}` | `T_COMMIT` in checks with secrets |
| Docker buildx and build-push-action | buildah in its own image, as root in t's sandbox without a daemon (chroot isolation, vfs): `tools/ci/publish/oci-build.sh` |

`tools/ci/publish/` holds the scripts the checks run, so the same steps run on t, on a laptop or
anywhere else: `oci-build.sh` builds an image into an OCI archive and records its manifest
digest; `oci-sign.sh` signs that digest with the KMS key (Cosign's simple-signing payload,
signed as a blob, so the signer needs no registry access); `oci-publish.sh` pushes the archive
with its digest unchanged, attaches the signature, verifies it against `keys/cosign.pub`, tags
it and writes a digest receipt, logging in to the registry as `oidc` with a `registry.tilo.so`
identity token. `os/dev/configure-cosign-kms.sh` turns an identity token into the AWS signing
role for Cosign, from t (audience `sts.amazonaws.com`) as from Forgejo Actions; it parses with
sed, since the skopeo image the checks run in has no jq.

Images built natively per architecture become one multi-platform image with `oci-index.py`, which
joins their archives into one holding an image index; its digest is fixed by the builds, so the
same signing and publishing scripts handle it. `oci-run.sh` runs a command inside a built
archive (buildah, no daemon), which is how image tests run between building and publishing, and
`oci-anonymous.sh` proves a published image can be pulled with no credentials.

Images that release builds verify with a plain `cosign verify --key keys/cosign.pub` (the
kernel buildroot, the env-runtime, the relay) are signed with `TILO_SIGN_TLOG=1`: `oci-sign.sh`
then also enters the signature in the public transparency log (Rekor) and keeps the entry as
`bundle.json`, which `oci-publish.sh` attaches with the signature, since such a verification
requires a log entry. The signing check still needs no registry access.

Repositories installed systems verify through their container policy (the OS image and the
legacy guest, `device_verified` in `lib.sh`) need more once the release root exists
(`keys/release/root.crt`, see `keys/README.md`): their policy accepts only a signature whose log
entry names an operational certificate the root issued. `cosign sign-blob` cannot log a
certificate, so `oci-sign.sh` also signs once per certificate in `keys/operational/` through
`tools/signing/sign-blob-certified.sh`, which enters the certificate in Rekor itself and keeps
`certified/N.{sig,bundle,crt,epoch}`; `oci-publish.sh` attaches each with its certificate and the
root, and verifies it through the chain as installed systems do.

## Products

Every container image Tilo publishes runs the same chain, so a product is a few lines of data in
`tools/ci/products.toml`, not a page of checks. `python3 tools/ci/t_toml.py write` turns each
`[[product]]` into its checks in `t.toml`, between `# BEGIN generated … products-<family>` markers;
the policy tests fail while `t.toml` is out of date. The chain:

| check | what it does | when |
|---|---|---|
| `image-<name>-context` | prepares the build context (`[product.context]`) | asked for, and before every build |
| `image-<name>`, or `image-<name>-<arch>` per architecture | builds the image into `image.oci.tar` and `digest` and runs its tests | asked for, and before every publication |
| `image-<name>` with `index = true` | joins the architectures' archives into one image index (`oci-index.py`) | |
| `[product.release_build]`'s `name` | derives the image a release publishes from the build's, into `<dir>/release` | `release_on` (stable) |
| `sign-<name>` | signs the digest the build produced with the release key in KMS (`oci-sign.sh`) | `release_on` (stable) |
| `publish-<name>` | pushes the archive with its digest unchanged and writes `receipt.json` (`oci-publish.sh`) | main for `dev` tags, `release_on` for `release` tags |

Development and release are one chain: `publish-<name>` names `sign-<name>` in `optional_needs`,
so on main it runs without a signature and pushes the `dev` tags, and on stable it waits for the
signature and pushes the `release` tags (`oci-publish.sh any … dev:<tag> release:<tag>`; the
script takes the run's channel from `T_REF` and refuses a release without its signature). A
product with only `dev` tags is never signed; one with only `release` tags publishes nowhere
else. Build steps hold no secrets and run for a working tree with `t test image-<name>`; the
signing and publishing steps only read what the build produced, so they run on any architecture
(`requires = ["arch=any"]`).

A product:

```toml
[[product]]
family = "services"                      # one generated block per family
name = "web-entry"                       # check names: image-web-entry, publish-web-entry, …
about = "The web entry."                # the comment above its checks
repository = "registry.tilo.so/tilo/web-entry"
dev = ["dev-{commit}"]                   # tags main pushes; leave out to publish no development builds
release = ["sha-{commit}"]               # tags a signed release pushes; {version} on v* tags
# release_on = ["tag:v*"]                # where releases come from (default: stable)
# arches = ["x86_64", "aarch64"]         # built natively, one check each (default: x86_64)
# index = true                           # publish one multi-platform index of them
# tlog = true                            # enter the signature in the transparency log
# gates = ["web-entry-e2e"]              # checks that must pass before signing and publishing
# anonymous = true                       # prove the publication pulls without credentials
# dev_signed = true                      # sign development candidates with the development key
# dispatch = true                        # publish development tags only when dispatched
# children = true                        # with index: sign and publish each image too, tagged
#                                        # <first tag>-<arch> (consumers that verify each one)

[product.context]                        # optional: a step that prepares the build context
run = "bash tools/ci/publish/web-entry-context.sh {dir}/context /target/publish/web-desktop/bundle"
inputs = ["crates/browser/tilo-browser-runtime/container/**"]
needs = ["web-desktop-bundle"]           # checks whose outputs the context takes
outputs = ["{dir}/context/**"]

[product.build]
context = "{dir}/context"                # or `run = "…"` for a build script of its own
containerfile = "{dir}/context/Containerfile"
# test = ["bash <dir>/test-image.sh {out}/image.oci.tar"]  # commands that test the archive

# [product.release_build]                # optional: the image a release publishes, derived
# name = "carry-web-entry"               # from the build's on the release references only
# run = "bash tools/ci/publish/web-entry-carry.sh {out} {dir}/release"
```

Tags may name `{commit}`, `{version}` (on `v<version>` tags), `{rust}` (the channel
`rust-toolchain.toml` pins) and `{fingerprint}` (the published directory's `fingerprint` file). A
tag written `once:<tag>` never moves: where it exists already, nothing is published.

A product with `[product.provenance]` (`run`, `inputs`, `outputs`) gets a `provenance-<name>`
check, no secrets, that writes `provenance.json` beside the index (or each image): the SLSA
predicate, naming the image's digest as `expectedReleaseDigest` and the commit as `{commit}`.
`sign-<name>` then also signs it as an in-toto attestation for that digest (`oci-attest.sh`,
`cosign attest-blob` with the KMS key, in the transparency log, so the product needs `tlog`),
and `oci-publish.sh` attaches it and checks it with `cosign verify-attestation`, as the
consumers do. A `dev_signed` product's development candidates are attested the same way with the
development key (`keys/cosign-dev.pub`), without a log entry; beside a release, its signing
check on main is `sign-dev-<name>`.

`{dir}` is `/target/publish/<name>`, `{out}` the build's directory (`{dir}`, or `{dir}/<arch>`
per architecture), `{arch}` and `{platform}` (`linux/amd64`) the architecture built. Any other
field of `[product.context]` and `[product.build]` (`inputs`, `requires`, `requirement`,
`timeout_secs`, `needs`, `privileged`, `cargo_packages`, …) goes to the check as written; the
generator adds the publishing scripts to `inputs`, `arch=<arch>` to `requires` and `default =
false`, and writes `on`, `id_token` and the signing environment itself, so a product can't
publish from a working tree by mistake.

A release whose image differs from the one the build tested names a step that derives it,
`[product.release_build]`: the web entry's release carries the version of the image production
serves (`carry-web-entry`), which only stable, the reference production deploys, needs. The step
runs on `release_on` without secrets, `needs` the build and writes `<dir>/release/image.oci.tar`
and `digest` (and a `receipt-extra.json`, one JSON object whose members `oci-publish.sh` adds to
the receipt). `sign-<name>` signs that archive, and `publish-<name>` names the step in
`optional_needs` and publishes it on the release channel (`TILO_RELEASE_OUT`), still writing the
receipt to `<dir>`; main's development tags are the build. Other fields go to the check as for a
build (`cache = false` for a step that observes the outside world).

To add a product: add its table, run `python3 tools/ci/t_toml.py write`, then `t test
image-<name>` to build and test it from your tree. The trusted halves run only on main,
stable and tags, so try them locally in their dry-run mode, which signs with a throwaway key and
publishes into a local OCI layout instead of the registry and KMS:

```sh
export TILO_PUBLISH_DRY_RUN=$PWD/target/dry-registry T_COMMIT=$(git rev-parse HEAD)
t outputs image-web-entry                # target/publish/web-entry/{image.oci.tar,digest}
T_REF=stable bash tools/ci/publish/oci-sign.sh registry.tilo.so/tilo/web-entry target/publish/web-entry
T_REF=stable bash tools/ci/publish/oci-publish.sh any registry.tilo.so/tilo/web-entry \
  target/publish/web-entry 'dev:dev-{commit}' 'release:sha-{commit}'
cat target/publish/web-entry/receipt.json
```

Chains that are not one container image (the apps, the desktop packages, the Windows imager,
the env tools) differ between architectures only by the architecture, so they are written once
as a `[[matrix]]`: each `[[matrix.check]]` is an ordinary check whose fields may name the
placeholders of `each`, written once per variant. One with `only = ["x86_64"]` is written for
those variants only, one whose name has no placeholder once after them, and `fields` go to every
check that doesn't set them:

```toml
[[matrix]]
family = "windows"
each = [{ arch = "x86_64" }, { arch = "aarch64" }]
fields = { image = "registry.tilo.so/tilo/tilo-builder@sha256:…" }

[[matrix.check]]
name = "build/windows-{arch}"
run = "bash tools/ci/windows/build.sh imager {arch} /target/windows/sdk/sdk-{arch}.tar.zst /target/windows/{arch}/build"
needs = ["build/windows-sdk-{arch}"]
```

A variant can carry more than the architecture where names differ (`deb = "arm64"`,
`node = "arch=arm64"`).

`tools/tests/test_publish_chain.py` covers the generator and the scripts' channel rules.

## Rehearsing a release

`release-rehearsal` (in `tools/ci/products.toml`) is a one-file image
(`tools/ci/publish/rehearsal`) that runs the chain every product runs, so a release can be
proven on t without releasing anything anyone deploys:

- `t dispatch publish-release-rehearsal --ref main` publishes
  `registry.tilo.so/tilo/release-rehearsal:dev-<commit>` and sets main's `release-rehearsal`
  pointer to the receipt (pushes to main don't, since nothing deploys it);
- a tag `v0-rehearsal-<n>` runs it as a release, and nothing else runs on such a tag (the relay
  releases on version tags only, `v1.2.3`): `sign-release-rehearsal` exchanges a
  `sts.amazonaws.com` identity token for the signing role and signs the build's digest with the
  release key in KMS, `publish-release-rehearsal` pushes `sha-<commit>` with the signature
  attached, verifies it against `keys/cosign.pub`, writes the receipt and sets the tag's
  `release-rehearsal` pointer. `t dispatch publish-release-rehearsal --ref stable` does the same
  on stable's head.

```sh
git tag v0-rehearsal-2 origin/main && git push origin v0-rehearsal-2
t main          # published: sign-release-rehearsal, publish-release-rehearsal; pointers
cosign verify --insecure-ignore-tlog=true --key keys/cosign.pub \
  registry.tilo.so/tilo/release-rehearsal:sha-$(git rev-parse v0-rehearsal-2)
```

The same chain runs against a local t server without any credential: give the server
`[env] TILO_PUBLISH_DRY_RUN = "/tmp/dry-registry"`, an `[oidc]` table and a worker that runs
micro-VM steps, then `t dispatch publish-release-rehearsal --ref stable`. Signing then uses a
throwaway key and publishing an OCI layout in the step.

## Mirrored images

The images the publishing checks run in (buildah, skopeo) and the bases t's own web interface
builds from are mirrored into `registry.tilo.so/tilo/<name>` and pinned there by digest:
quay.io rebuilds its version tags and deletes the manifests they replace, so a pin on quay.io
stops resolving within days. `tools/ci/publish/mirrored-images.tsv` lists each mirror, its
upstream tag and the digest; `mirror-images`, on main with a registry identity token, copies each
digest across with `--preserve-digests` (the mirror's digest is upstream's) while upstream still
serves it. To move one forward, put the new digest in that file and in the pins in one commit.

## Declared in t.toml

| chain | build (no secrets) | sign (stable) | publish (main, development tag) | release (stable) |
|---|---|---|---|---|
| room service | `image-room-service-context`, `image-room-service` | `sign-room-service` | `publish-room-service` (dispatch): `dev-<commit>` | `publish-room-service`: `sha-<commit>`, signed, receipt |
| web entry | `image-web-entry-context`, `image-web-entry` | `sign-web-entry` | `publish-web-entry` (dispatch): `dev-<commit>` | `publish-web-entry`: `sha-<commit>`, signed, receipt |
| release rehearsal | `image-release-rehearsal` | `sign-release-rehearsal` (`v0-rehearsal-*` tags, dispatch) | `publish-release-rehearsal` (dispatch): `dev-<commit>` | `publish-release-rehearsal`: `sha-<commit>`, signed, receipt, the `release-rehearsal` pointer |
| mirrored images | | | `mirror-images`: the digests of `tools/ci/publish/mirrored-images.tsv` | |
| ci-fast | `image-ci-fast`, `image-ci-fast-smoke` | | `publish-ci-fast`: `sha-<commit>` | (CI tooling, not a product) |
| registry server | `image-registry-server` (compiled in the Containerfile, then its image test) | `sign-registry-server` | `publish-registry-server` (dispatch): `dev-<commit>` | `publish-registry-server`: `sha-<commit>`, signed, receipt for GitOps |
| relay | `rpm-snapshot` (the published frozen snapshot, verified), `image-relay` | `sign-relay` (`v*` tags, with a transparency log entry) | | `publish-relay` (`v*` tags): `<version>`, `sha-<commit>`, and `latest` for a final version, signed |
| env-runtime | `rpm-snapshot`, `image-env-runtime-tree-x86_64`, `-aarch64`, `env-runtime-packer`; `env-runtime-preview`, `image-env-runtime-preview` (sealed with a throwaway key) | `seal-env-runtime` (publisher identity), `sign-env-runtime` (`image-env-runtime` between them) | | `release-env-runtime`: `set-<fingerprint>` once, signed |
| env tools | `env-tools-binaries-x86_64`, `-aarch64`, `env-tools-x86_64`, `-aarch64` (every tool sealed for the runtime's publisher with a placeholder signature); `image-env-tools-preview` (the image of the unsigned builds); on stable `env-tools-plan` first, after `release-env-runtime`: the tag, and nothing built where it is published | `seal-env-tools` (publisher identity over the built tools, compiling nothing; the catalogs rewritten to name the signed ids), `sign-env-tools` (with a transparency log entry; `image-env-tools` between them) | | `publish-env-tools`: `<runtime tag>-<catalog fingerprint>` once, signed |
| RPM snapshot | `image-rpms-x86_64`, `-aarch64`, `image-rpms` (their index), `provenance-rpms` (frozen-rpm-index/v1, the fingerprint) | `sign-rpms`: the index and both images, the index's provenance, with transparency log entries | `mirror-base`: the pinned Fedora bootc base copied into registry.tilo.so while upstream serves it, before the builds | `publish-rpms`: `set-<fingerprint>` once, signed and attested |
| kernel | `image-kernel-cache-<arch>` (the compiler cache, keyed by the buildroot), `image-kernel-x86_64`, `-aarch64` (through it; FROM the buildroot of the machine it builds on), `kernel-acceptance-<arch>` (on the kernel's architecture), `provenance-kernel` (kernel-rpm/v1, binding the buildroot and the build architecture) | `sign-kernel`: both, with provenance and transparency log entries | | `publish-kernel`: `sha-<commit>-<arch>` once, the `kernel` pointer; `pin-kernel` commits it to `os/kernel-snapshot.env` on main |
| kernel buildroot | `image-kernel-buildroot-x86_64`, `-aarch64` (built and audited natively) | `sign-kernel-buildroot` (both) | | `publish-kernel-buildroot`: `sha-<commit>-<arch>`, signed, the `kernel-buildroot` pointer |
| CI builder | `image-ci-builder-x86_64`, `-aarch64`, `image-ci-builder` (their index); in each candidate `canary-ci-builder-cache-cold-<arch>`, `-cache-warm-<arch>` (t's compiler cache, written by one job and read back by another) and `canary-ci-builder-browser-<arch>`, all of which gate signing and publishing | `sign-ci-builder`: the index and both images, with transparency log entries | `sign-dev-ci-builder` (development key), `publish-ci-builder`: `dev-<commit>`, `dev-<commit>-<arch>` | `publish-ci-builder`: `<commit>`, `<rust channel>`, signed; `latest` is not moved yet |
| release tools | `image-release-tools-x86_64`, `-aarch64` (each built and tested natively), `image-release-tools` (their index) | | `publish-release-tools` (dispatch): index `sha-<commit>`, proven anonymously pullable | (CI tooling, not a product) |
| local model catalog | `model-catalog-compiler`, `local-model-runtime-*` (the runtimes, stable) | `sign-model-catalog`, `review-model-catalog` (dispatch) | | `publish-model-catalog` (dispatch): the signed catalog and the runtime archives in generic packages |
| cloud deploy | | | | `cloud-deploy` (stable, and dispatch on stable): after `publish-room-service` and `publish-web-entry`, commits their receipts' digests to the GitOps repository's main as the deploy target there says, waits until the services report their new build and stay healthy, and otherwise commits the previous digests back; a dry run while the target says `rollout = false` |
| canonical verifier bundles | `verifier-bundle-<arch>` (`on = ["dispatch"]`, `t outputs`) | | | (reviewed and uploaded by hand) |
| apps | `apps-build-x86_64`, `apps-build-aarch64`: every app of the default collection, packaged with throwaway keys, smoke-tested | `sign-apps-x86_64`, `sign-apps-aarch64`: the publisher identities over the built binaries, compiling nothing | | `publish-apps` (package token): tapps, collections, runtime objects, the `app-collection` pointer; `submit-apps` (publisher identity): the registry entries |
| engine | `engine-build-x86_64`, `engine-build-aarch64`: the pinned CEF host and the release tools, sealed with throwaway keys (from a committed tree) | `sign-engine-x86_64`, `sign-engine-aarch64`: the runtime and app publisher identities over the built files, compiling nothing | | `upload-engine` (package token): the runtime package `so.tilo.runtime.chromium`; `submit-engine` (publisher identity): its registry entry |
| desktop | `desktop-preflight` (packaging inputs), `desktop-package-amd64`, `desktop-package-arm64` (privileged), `host-session-e2e` (the amd64 RPMs logged into on stock Fedora under KVM; stable and dispatch) | | | `publish-desktop-payload` (after `host-session-e2e`): the OS payloads, the `desktop-payload` pointer |
| macOS | `vm-macos`, `runtime-macos` (main and stable), `sdk-macos`, `apps-macos` (idle main and stable), `runtime-macos-app` (dispatch, ad hoc signed) | | | |
| Windows ARM64 | `windows-arm64-tilo-vm`, `windows-arm64-imager` (dispatch, cross-built) | | | |
| Windows compatibility payloads | `image-fex-buildroot` (aarch64), `image-fex-x86-sdk`, `image-fex-payload-context` (the verified FEX source), `image-fex-payload` (aarch64); `image-wine-tools`, then in it as micro-VM steps `wine-source` (the verified Wine source), `wine-resolve-x86_64`, `-i686` (Mock's build dependencies), `wine-build-x86_64`, `-i686` (Wine rebuilt offline), `image-wine-paired` (both targets' RPMs merged) and `image-windows-guest` (the pair signed with a key made for the run, the guest assembled) | | `publish-fex-payload` (dispatch, after `publish-fex-buildroot` and `publish-fex-x86-sdk`, which it names): the `fex-payload-candidate` pointer; `publish-windows-guest` (dispatch, after `publish-wine-tools`, with every image the guest names): the `windows-guest-candidate` pointer | (development candidates only) |
| OS images | `os-assembler-<arch>` (the image the OS actions run in), `os-base-<arch>`, `os-builder-<arch>`, `os-tools-<arch>`, `os-compile-<arch>` (os/image/compile.sh in the OS builder, with t's compiler cache and warm build directories), `os-layer-<layer>-<arch>` (one per layer of os/image/layers.toml, keyed by what its piece reads), `os-image-<arch>` (the layers composed, an OCI layout; no LTO, apps and desktop of the commit; `t outputs`; installed from the run's `image-rpms-<arch>`, unsigned, while the package set has no published snapshot), `os-dev-image-<arch>` (os/Containerfile.dev on it), `os-acceptance-<arch>` (stable, idle and dispatch: the dev image installed and its devices and desktop gated under KVM), `os-board-lenovo-83ed`, `os-board-orangepi-3b-v2-1` (os/Containerfile.board on the aarch64 dev image, sealed, and its flashable disk); the release image: `os-compile-release-<arch>` (thin LTO), its `release` layers, `os-release-image-<arch>`, `os-release-seal-<arch>` (stable and dispatch: checked inside, installed, which seals it, and booted to the greeter), `os-release-reproduce-<arch>` (built again from `os-release-recompile-<arch>`, without caches, to the same bytes), `os-release-rehearsal-<arch>` (the signing and publishing with throwaway keys into an OCI layout); a board's release image (`release_boards` in os/image/layers.toml: the Lenovo 83ED): its `board` layers built again with its hardware profile, `os-release-image-lenovo-83ed`, `os-release-seal-lenovo-83ed` (checked inside and installed, which seals it; not booted, QEMU cannot boot the board's device tree), `os-release-reproduce-lenovo-83ed`, `os-release-rehearsal-lenovo-83ed` | `sign-os-boot-<arch>` (the UKIs' EFI signatures and the PCR 11 prediction, KMS), `os-release-<arch>` (verified and added, with SBOM and provenance), `sign-os-release` (release key, provenance and freshness attestations); the same for the board: `sign-os-boot-lenovo-83ed`, `os-release-lenovo-83ed` (provenance binding the board, its hardware definition and firmware), `sign-os-release-lenovo-83ed` | | `publish-os-dev-<arch>` (dispatch on main): `tilo-ci:main-<commit7>-<arch>`, `tilo-vm:main-<commit7>-ci` and its disk, `tilo-vm:main-ci` and `latest` moved forward; `publish-os-board-<board>` (dispatch): `tilo-vm:main-<commit7>-<board>`, `tilo-vm:<board>-ci` and the board's flashable disk moved forward along main | `publish-os-release` (dispatch on stable or `v*`): `tilo:gen<N>` once, `<version>`, `latest`; `publish-os-release-lenovo-83ed` (dispatch on stable or `v*`, with `REGISTRY_TOKEN`: the forge's registry takes no identity token): `code.tilo.so/tilo/tilo:lenovo-83ed-<commit>` and `lenovo-83ed-gen<N>` of the board's own series once, then `lenovo-83ed-latest` |
| system-extension manifests | `extension-manifests-x86_64`, `-aarch64` (daily and dispatch, resolved inside each published image) | `publish-extension-manifests` (extension-manifest key) | | |

`t test image-room-service` builds the image from your tree exactly as a release would, without
publishing anything. The publishing checks run only as trusted runs; `t test publish-room-service`
on a working tree reports them skipped ("needs identity tokens: runs on …") after running their
builds.

## Moved to t

Forgejo Actions no longer runs for this repository, so a workflow is deleted once t's checks do
its work. Where their trusted half has passed from main, stable or a tag, the evidence is the run
on prod t.

Deleted:

| workflow | replaced by | evidence on t |
|---|---|---|
| `release-registry-server.yml` | `image-registry-server`, `publish-registry-server` | `publish-registry-server` on main 291d4d45c1 pushed `tilo-registry-server:sha-291d4d45c1…` |
| `build-release-tools.yml` | `image-release-tools-x86_64`, `-aarch64`, `image-release-tools`, `publish-release-tools` | builds in r5072cc14f4; `publish-release-tools` on main 0fcb7c612d pushed the index `sha-0fcb7c612d…` |
| `release-verifier-bundles.yml` | `verifier-bundle-x86_64`, `-aarch64` (dispatch) | rfad5de2f14, r352e0cb508 |
| `build-ci-fast-images.yml` | `image-ci-fast`, `image-ci-fast-smoke`, `publish-ci-fast` (ci-control, which only `build-image.yml` ran in, retired with it) | `publish-ci-fast` and `publish-ci-control` on main ad95cf21a4, after the smoke test |
| `room-service-image.yml` | `image-room-service-context`, `image-room-service` (its smoke test on the archive), `sign-room-service`, `publish-room-service` | the image and its test r3d39f0a8e5; `publish-room-service` on main a38941e29a; the release chain on rehearsal tags |
| `web-entry.yml` | `image-web-entry-context`, `image-web-entry` (smoke test on the archive), `carry-web-entry` (stable: carries the version production serves), `sign-web-entry`, `publish-web-entry` | `publish-web-entry` on main ad95cf21a4; context, image and carry re77dfb7adf; the release chain on rehearsal tags |
| `build-windows-compat.yml` | the windows-compat chain (`image-wine-tools`, `wine-*`, `image-wine-paired`, `image-windows-guest`, `image-fex-*`, `sign-fex-payload`, `publish-*`) | dispatches on main: r766c31687a (FEX payload, signed with the development key), r5b5e69f29c (windows guest); both pointers set |
| `ci-development-signing.yml` | `development-signing-context-<arch>`, `development-signing-<arch>` (micro-VM steps, on stable and dispatched) | ref31a8cb9a, both architectures |
| `extension-manifests.yml` | `extension-manifests-<arch>`, `publish-extension-manifests` (daily and dispatched on main) | r7b7f62f1ce: resolved on both architectures, signed through TiloTExtensionManifestSigner, published with `TILO_PACKAGES_TOKEN` |
| `check.yml` | the `test/<package>`, `test-x86_64/<package>`, `clippy/<package>`, `check/<package>` and `goldens/<package>` families, `suite-<group>`, `security`, `workspace-layout`, `certify`, `example-scenarios`, `android`, `wasm`, `web-examples`, `vendored`, `pins`, `portable`, `visual-goldens`, `app-sizes-<arch>` | checks in t.toml; Forgejo Actions no longer runs |
| `lint.yml` | `fmt`, `hakari`, `changes`, `policy/<step>`, `app-lints`, `deny`, `machete` | checks in t.toml; Forgejo Actions no longer runs |
| `app-sdk.yml` | `sdk-policy`, `sdk-linux`, `sdk-web`, `sdk-web-size` | checks in t.toml; Forgejo Actions no longer runs |
| `sdk-native-envelope.yml` | `sdk-envelope` | checks in t.toml; Forgejo Actions no longer runs |
| `web-size-native-envelope.yml` | `web-size-envelope-bundle`, `web-size-envelope` | checks in t.toml; Forgejo Actions no longer runs |
| `wasm-check-qualification.yml` | `wasm`, `web-examples` | checks in t.toml; Forgejo Actions no longer runs |
| `security-isolation.yml` | `security-isolation` | checks in t.toml; Forgejo Actions no longer runs |
| `ci-render-counters.yml` | `render-counters` | checks in t.toml; Forgejo Actions no longer runs |
| `standalone-app.yml` | `standalone-app` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-apps-macos.yml` | `sdk-macos`, `apps-macos` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-runtime-macos.yml` | `runtime-macos` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-windows-arm64.yml` | `windows-arm64-tilo-vm`, `windows-arm64-imager` (dispatch, cross-built: [Windows](#windows)) | checks in t.toml; Forgejo Actions no longer runs |
| `dev-visual-baselines.yml` | `visual-baselines` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-mounted-reopen.yml` | `mounted-reopen` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-source-metadata.yml` | none: it existed for check.yml's affected-package planner, which went with it | retired with the planner |
| `dev-policy-profile.yml` | `policy-profile` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-source-blobs.yml` | `source-blobs` | checks in t.toml; Forgejo Actions no longer runs |
| `dev-focused-test.yml` | `t test test/<package> --test <target>`, `check/<package>`, `clippy/<package>` ([t.md](t.md)) | checks in t.toml; Forgejo Actions no longer runs |
| `dev-ci-checkout.yml` | none: it measured checkouts on Forgejo runners | obsolete |
| `dev-imager-windows-native.yml` | none: a scaffold for a Windows runner that was never enabled | obsolete |
| `inspect-rpm-signing.yml` | none: it inspected the signing broker's client on a Forgejo runner | obsolete |
| `ci-weekly-quality.yml` | `weekly-warnings`, `weekly-clippy`, `weekly-docs`, `weekly-release`, `weekly-latency` | checks in t.toml; Forgejo Actions no longer runs |
| `perf-room-capacity.yml` | `perf-room-capacity` | checks in t.toml; Forgejo Actions no longer runs |
| `browser-auth-e2e.yml` | `browser-auth-e2e` | checks in t.toml; Forgejo Actions no longer runs |
| `ci-nightly.yml` | `nightly-fuzz`, `nightly-workspace-test`, `nightly-arm64-workspace-test`, `nightly-arm64-goldens`, `nightly-live-music`, `nightly-check`, `nightly-arm64-check`, `nightly-arm64-certify`, `nightly-examples`, `nightly-arm64-examples`, `nightly-ignored`, `nightly-arm64-ignored`, with `vendored`, `fmt`, `certify`, `workspace-layout` and the goldens | checks in t.toml; Forgejo Actions no longer runs |
| `ci-staging-loop.yml` | `staging-loop` | checks in t.toml; Forgejo Actions no longer runs |
| `release-local-model-catalog.yml` | `model-catalog-compiler`, `review-speech-catalog`, `sign-model-catalog`, `publish-model-catalog` (dispatch on stable) | checks in t.toml; Forgejo Actions no longer runs |
| `release-relay.yml` | `rpm-snapshot`, `image-relay`, `sign-relay`, `publish-relay` (version tags) | `rpm-snapshot` and `image-relay` passed; checks in t.toml; Forgejo Actions no longer runs |
| `build-buildroots.yml` | `image-kernel-buildroot-<arch>`, `sign-kernel-buildroot`, `publish-kernel-buildroot`, `pin-kernel-buildroot` | both architectures built; `pin-kernel-buildroot` ran on main; checks in t.toml; Forgejo Actions no longer runs |
| `build-env-runtime.yml` | `image-env-runtime-tree-<arch>`, `env-runtime-packer`, `env-runtime-preview`, `image-env-runtime-preview`, `seal-env-runtime`, `image-env-runtime`, `sign-env-runtime`, `release-env-runtime` | both trees, the packer, a throwaway seal and the index built; checks in t.toml; Forgejo Actions no longer runs |
| `build-kernel.yml` | `image-kernel-<arch>`, `kernel-acceptance-<arch>`, `provenance-kernel`, `sign-kernel`, `publish-kernel`, `pin-kernel` | builds and acceptance on both architectures (re81dd6df90); checks in t.toml; Forgejo Actions no longer runs |
| `build-legacy-guest.yml` | `legacy-guest-agent`, `image-legacy-guest`, `legacy-guest-e2e-binaries`, `legacy-guest-e2e`, `sign-legacy-guest`, `publish-legacy-guest`, `pin-legacy-guest` | image and end-to-end test (r8673ba882c), the pin on main; checks in t.toml; Forgejo Actions no longer runs |
| `build-ci-builder.yml` | `image-ci-builder-<arch>`, `image-ci-builder`, `canary-ci-builder-*-<arch>`, `sign-ci-builder`, `sign-dev-ci-builder`, `publish-ci-builder` | both architectures built; checks in t.toml; Forgejo Actions no longer runs |
| `build-rpms.yml` | `image-rpms-<arch>`, `image-rpms`, `provenance-rpms`, `sign-rpms`, `publish-rpms`, `rpm-snapshot` | both architectures built; checks in t.toml; Forgejo Actions no longer runs |
| `host-session-e2e.yml` | `host-session-e2e` (needs `desktop-package-amd64`; records each RPM's SHA-256 in `source.json`), which `publish-desktop-payload` needs on stable; the replay of another run's RPMs is the cached lane | dispatched on main (r8b02906d73): not passed yet, since `desktop-package-amd64` fails in its arch, nix and fedora-44 userspaces (the Fedora app store refuses the Browser without its pinned engine runtime) |
| `release-imager-winget.yml` | `imager-winget-acceptance` (every six hours and dispatch, `cache = false`), with the site token as the secret `TILO_SITE_TOKEN` instead of a broker lease | skipped by t ("needs secret TILO_SITE_TOKEN, which this server does not have") until the server config has it (rf0c329cba6); the run itself refuses to start without it |
| `ci-filesystem.yml` | `real-fuse`, `real-fuse-privileged`, `files-ux`, `native-tier1`; the cloud-source conformance suite's server layers as `cloud-conformance-build`, `cloud-servers` (containers in a micro-VM step) and `cloud-live` (live accounts, nightly) | `cloud-servers` r0e79c3d184 (38 tests against real WebDAV and Nextcloud); `cloud-live` dispatched on main r24d0e7c476 (38 tests against the live accounts) |
| `ci-legacy-apps.yml` | `legacy-apps-build`, `legacy-apps` (nightly and dispatched, micro-VM step) | r0e79c3d184: GNOME Text Editor, Qt 6 and LibreOffice Writer passed |
| `build-env-tools.yml` | `env-tools-binaries-<arch>`, `env-tools-<arch>`, `env-tools-plan`, `seal-env-tools`, `image-env-tools`, `image-env-tools-preview`, `sign-env-tools`, `publish-env-tools` | binaries and catalogs on both architectures (r8673ba882c), sealed for the publisher and imaged from a working tree (r26f07821f0); checks in t.toml; Forgejo Actions no longer runs |
| `resolve-builder.yml` | none: a reusable workflow that looked the CI builder up for the workflows above, retired with the last of them; `tools/ci/resolve_builder.py` stays for `release-apps.yml` | obsolete |

## Inventory

Every workflow that publishes, signs or releases, with what it needs and where it goes. "OIDC"
means the job asks Forgejo for an identity token (`enable-openid-connect`): to assume the AWS
signing role for Cosign (`os/dev/configure-cosign-kms.sh`), to obtain a lease from a credential
broker installed on the runner, or as a Forgejo API token. On t these become t's identity
tokens wherever the other side can trust t's issuer (the registry and AWS already do), and
fixed credentials in the server config, limited by reference and node, only where it can't.

### Container images

| workflow | trigger | secrets | runner needs | outputs | on t |
|---|---|---|---|---|---|
| ~~`room-service-image.yml`~~ | push main (paths), PR build only, dispatch | `GITHUB_TOKEN` (registry push) | x86_64, Docker buildx | `tilo/room-service:sha-<sha>`, digest receipt | moved to t (above) |
| ~~`web-entry.yml`~~ | push main (paths), PR, dispatch | `GITHUB_TOKEN` | x86_64, Docker buildx; reads the version production serves | `tilo/web-entry:sha-<sha>` carrying the replaced image's version (`container/carry.sh`), receipt | moved to t (above) |
| ~~`build-ci-builder.yml`~~ | weekly, push main (paths), dispatch | `COSIGN_KEY_REF`, `AWS_SIGNING_ROLE_ARN` (OIDC), `GITHUB_TOKEN` | x86_64 and arm64 natively, buildx with gha cache, canary jobs that run inside the candidate image | `tilo/tilo-builder:<sha>`, `:<rust>`, `:latest`, signed index | moved to t (above) |
| ~~`release-relay.yml`~~ | push main and `v*` (paths), PR, dispatch | Cosign via OIDC, `GITHUB_TOKEN` | x86_64, buildx with host network for the compiler cache; waits for `build-rpms.yml` by polling the Forgejo API | `tilo/tilo-relay:<semver>`, signed, on tags only | moved to t (above) |
| ~~`build-rpms.yml`~~ | push main (paths), dispatch | Cosign via OIDC, `GITHUB_TOKEN` | x86_64 and arm64, Docker daemon | `tilo/rpms:set-<fingerprint>` signed index with SLSA provenance | moved to t (above) |
| ~~`build-buildroots.yml`~~ | push main (paths), dispatch | Cosign via OIDC, `RELEASE_TOKEN` | per kind and arch, buildx | `tilo/tilo-kernel-buildroot`, and a pull request pinning `os/kernel-buildroot-snapshot.env` | moved to t (above) |
| ~~`build-kernel.yml`~~ | push main and `v*`, PR, dispatch | Cosign via OIDC, `RELEASE_TOKEN` | `disk-100g` x86_64 and arm64, KVM to boot the kernel and probe Secure Boot | `tilo/tilo-kernel` tags per arch, signed with provenance, pin pull request | moved to t (above) |
| ~~`build-legacy-guest.yml`~~ | push main (paths), PR, dispatch | Cosign via OIDC, `RELEASE_TOKEN` | `disk-100g` x86_64, root podman, loop devices, KVM | `tilo/legacy-guest` signed, pin pull request | moved to t (above) |
| ~~`build-env-runtime.yml`~~ | every 4 hours, push main (paths), dispatch | Cosign via OIDC, `GITHUB_TOKEN`, `TAPP_PUBLISHER_IDENTITY` (raw Ed25519 identity) | x86_64 and arm64, Docker daemon | `tilo/env-runtime:set-<fp>`, sealed with the tapp publisher identity, signed | moved to t (above) |
| ~~`build-env-tools.yml`~~ | every 6 hours, push main (paths), dispatch | as env-runtime | privileged container, loop-mounted ext4 | `tilo/env-tools:<runtime>-<catalog>` signed | moved to t (above) |
| ~~`build-windows-compat.yml`~~ | dispatch | Cosign via OIDC, `GITHUB_TOKEN` | Docker daemon, `disk-100g` Windows guest, arm64 FEX | `wine-tools`, `wine-paired`, `tilo-fex-*-candidate`, signed FEX payload | moved to t (above) |

### The OS

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| ~~`build-image.yml` (Release / OS)~~ | `v*` tags; dispatch (release, boards, firmware, app collection, desktop payload); called by `os-dev*.yml` for development builds | Cosign via OIDC; `RUNTIME_BUNDLE_SIGNING_KEY`, `TAPP_PUBLISHER_IDENTITY`, `DEV_APP_PUBLISHER_IDENTITY` (raw keys); `REGISTRY_TOKEN`; the EFI/PCR signer program `TILO_PCR_PREDICTION_SIGNER` on release runners; repository variables for the app collection and desktop payload | x86_64 and arm64 natively, buildx, root podman, loop and FUSE, KVM for the install and boot gates, 100 GiB scratch | development: `tilo-ci`, `tilo-vm` tags and disks; release: `tilo:gen<N>`, `:<semver>`, `:latest`, signed with SBOM, SLSA and freshness attestations, board images | moved to t: the `os-*` checks build, `os-acceptance-<arch>` and `os-release-seal-<arch>` gate, `publish-os-dev-<arch>` and `publish-os-board-<board>` publish development images and disks, `publish-os-release` the signed release and `publish-os-release-lenovo-83ed` the Lenovo board's (above); not ported: the rechunked layout (os/dev/rechunk-image.sh needs a BuildKit builder, which t does not provide) |
| ~~`os-dev.yml`, `os-dev-x86.yml`, `os-dev-arm.yml`~~ | PR with a label, dispatch | none (called workflows get no OIDC) | as above | development images | retired: `os-dev-image-<arch>` builds the development image |
| ~~`dev-disk.yml`~~ | dispatch | `GITHUB_TOKEN`, `REGISTRY_TOKEN` | `disk-100g` both arches, KVM | `tilo-vm` disks and `latest` (from main or `v*` only) | retired: `publish-os-dev-<arch>` publishes the gated dev disks |
| `os-dev-installer.yml`, `dev-disk-boot-diagnostic.yml` | dispatch | none | arm64/`disk-100g`, KVM, loop | artifacts only | `dev-installer-medium-<arch>` builds the self-install medium without capture or signing inputs (`t outputs`); the Windows-direct variants still need their own checks |
| ~~`extension-manifests.yml`~~ | daily, dispatch | `TILO_PACKAGES_TOKEN`; an identity token for the extension-manifest audience, whose role holds the KMS key | podman with the OS image, both arches | signed manifests in generic packages | retired: `extension-manifests-<arch>` resolves and `publish-extension-manifests` signs and publishes, on t's issuer and audience (`tilo-extension-manifests`, above) |

### Apps, desktop and models

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| `release-apps.yml` | push main (paths); `apps-v*` tags validate only; dispatch | `TAPP_PUBLISHER_IDENTITY`, `RUNTIME_BUNDLE_SIGNING_KEY` (raw keys), OIDC token as Forgejo API credential, writes repository variables `TILO_APP_COLLECTION_*` | x86_64 and arm64 builds in the builder image, llvmpipe smoke tests | tapps and collections in generic packages, registry entries at apps.tilo.so, the runtime package | the release chain is declared (above): on stable, from the full default collection, its release sequence the commit's time (above every run number used so far). `publish-apps` sets the `app-collection` pointer (`channel.json`: each architecture's collection URL and digest, the builder and the source), which replaces `TILO_APP_COLLECTION_*`. The workflow stays until a release OS image reads that pointer instead of the variables. The Browser's engine runtime, which apps name in `optional-runtimes` and the collections don't carry, is its own chain (`engine-build-*`, `sign-engine-*`, `upload-engine`, `submit-engine`, tools/apps/t-engine-release.sh), released from the same stable commit. Not ported: affected-app selection and reuse of the prior collection's members (they need the pointer), the coordinator qualification, the Signal-provider-only dispatch and the run 97894 recovery |
| `release-desktop.yml` | PR preflight, weekly, `desktop-v*` tags, dispatch | `REGISTRY_TOKEN`, `RELEASE_TOKEN`; RPM signing through a broker on the runner (OIDC) | distribution containers, a full clone for Nix | packages, a Forgejo release on tags, the OS payload and its repository variables | `desktop-preflight` is ported, but for the login wrapper's refusals, which need a user created in a throwaway container. The package lanes run each userspace in its own container, so `desktop-package-amd64` and `-arm64` are `privileged = true` (`dist/ci/package-lane.sh`). `publish-desktop-payload` publishes the Fedora lanes' OS payloads from stable and sets the `desktop-payload` pointer, which replaces `TILO_DESKTOP_PAYLOAD_*`. RPM signing: [below](#rpm-signing). The login test is `host-session-e2e`, which the payload's publication needs. Still in the workflow: the signed release on `desktop-v*` tags (it needs the RPM signer) and the weekly payload from main |
| ~~`host-session-e2e.yml`~~ | called by release-desktop, dispatch, PR | none | `disk-100g` x86_64, KVM | test result | moved to t (above) |
| ~~`release-local-model-catalog.yml`~~ | dispatch on main | `TILO_MODEL_CATALOG_SIGNING_KEY` (raw seed), `REGISTRY_TOKEN` | builder image | signed catalog in generic packages | moved to t (above) |
| ~~`release-verifier-bundles.yml`~~ | dispatch | none | x86_64 and arm64 | artifacts | ported: `verifier-bundle-<arch>` (above) |
| ~~`perf-scheduled.yml`, `vm-acceptance.yml`~~ (results) | daily | `REGISTRY_TOKEN` to append results | various | `tilo-perf-results` packages | retired: `perf-<group>` and `perf-scheduled`, `vm-acceptance` and `vm-acceptance-perf`, with `cache = false` and `TILO_PACKAGES_TOKEN`; the prebuilt cohort is `vm-acceptance-cohort` |
| ~~`cloud-promotion.yml`~~ | weekly drift report, dispatch | `INFRA_PROMOTION_TOKEN` | Git history | a promotion pull request on the infra repository | replaced by `cloud-deploy`, which deploys every new stable head through the forge's API, so it pushes nothing itself |
| ~~`check.yml`~~ (`checks` job) | push main | `REGISTRY_TOKEN` as a Git write token | | the CI receipt as a Git note | t's own commit statuses replace the receipt |

### The imager

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| `release-imager.yml` | `imager-v*` tags (signed release); dispatch (unsigned rehearsal) | brokers on the runners, each leased with OIDC: OpenPGP signing for Linux, Authenticode for Windows, Developer ID and notarization for macOS, GitHub App tokens for the public release repository, Homebrew tap and winget, a site token; Cosign; `REGISTRY_TOKEN`; the EFI/PCR signer | Linux x86_64 and arm64 with KVM and a render node, macOS on both arches, Windows on both arches | the GitHub release in `tilo-os/releases`, Homebrew, winget pull request, site downloads, installer media | Linux and macOS jobs as checks (`requires = ["os=macos", …]`), each signing step a separate check on `tag:imager-v*`; brokers become fixed credentials as secrets for release nodes; Windows: below |
| ~~`release-imager-winget.yml`~~ | every 6 hours | site token through the broker | Linux | the site's download state | moved to t (above) |
| `release-runtime-macos.yml` | dispatch (disabled) | macOS broker, update key | macOS arm64 | artifacts | shadowed on t, unsigned: `runtime-macos-app` (dispatch); Developer ID signing needs a release-only Mac and `dist/macos/build-app.sh` split into build and sign (dist/macos/README.md) |

### Not publishing

These only build or test, and become ordinary or `default = false` checks. Those t's checks
already run are deleted (the table [above](#moved-to-t)). Still here:
`dev-disk-boot-diagnostic.yml`.

## Identity tokens and secrets

Registry pushes and KMS signing use identity tokens from t (issuer `https://t.tilo.so`, formerly
`https://code.tilo.so/t`, both trusted while the old one retires;
subject `repo:tilo/tilo:ref:refs/heads/<branch>` or `…:ref:refs/tags/<tag>`). Which runs get
them is t's server config; what they may do is the other side's:

| audience | refs | accepted by |
|---|---|---|
| `registry.tilo.so` | `main`, `stable`, `tag:v*` | registry.tilo.so, as the password of the user `oidc`: tilo/tilo may push under `tilo/` from `main`, `stable` and `v*` tags |
| `sts.amazonaws.com` | `stable`, `tag:v*`, `tag:desktop-v*`, `tag:imager-v*` | the AWS role `TiloTReleaseSigner`, for `stable` and `v*` tags of tilo/tilo only: `kms:Sign`, `kms:GetPublicKey` and `kms:DescribeKey` on the three release keys |

More roles trust t's issuer the same way, each with its own keys:

| role | audience and references | keys |
|---|---|---|
| `TiloTPackageSigner` | `sts.amazonaws.com`; `stable`, `desktop-v*` and `imager-v*` tags | `alias/tilo-rpm-signing` (RSA 4096, digests only), `alias/tilo-authenticode-signing` (RSA 3072) |
| `TiloTExtensionManifestSigner` | `tilo-extension-manifests`; `main` | `alias/tilo-extension-manifests` |
| `TiloTDevSigner` | `tilo-dev-signing`; `main` | `alias/tilo-dev-signing` (P-256), development candidates only, never trusted for releases |

The key reference (`COSIGN_KEY_REF`) and the role (`AWS_SIGNING_ROLE_ARN`) are not secret; the
signing checks set them in `t.toml`. What has no OpenID Connect verifier yet stays a secret in
t's server config (sops-encrypted in the infra repository), none in this repository:

| name | refs | what |
|---|---|---|
| `TAPP_PUBLISHER_IDENTITY`, `RUNTIME_BUNDLE_SIGNING_KEY` | `stable`, `tag:*` | the app and runtime publisher identities, for the app and OS chains |
| `DEV_APP_PUBLISHER_IDENTITY` | `main` | the development app publisher identity, for development app collections from main |
| `TILO_MODEL_CATALOG_SIGNING_KEY` | `stable`, `tag:*` | the local model catalog's signing seed: `sign-model-catalog`, `review-model-catalog` |
| `REGISTRY_TOKEN` | `stable`, `tag:*` | a Forgejo token that may write packages, which do not take t's identity tokens: `publish-model-catalog`, and the Lenovo board's release in the forge's container registry, `publish-os-release-lenovo-83ed` |
| `INFRA_PROMOTION_TOKEN` | `stable` | the bot t-infra-promotion's token, which may write the infra repository: `cloud-deploy` reads the deploy target and commits the pins it moves |
| `TILO_LIVE_WEBDAV_URL`, `TILO_LIVE_WEBDAV_USER`, `TILO_LIVE_WEBDAV_PASSWORD`, `TILO_LIVE_NEXTCLOUD_URL`, `TILO_LIVE_NEXTCLOUD_USER`, `TILO_LIVE_NEXTCLOUD_PASSWORD` | `main` | the cloud-source conformance suite's live WebDAV and Nextcloud test accounts: `cloud-live`, which fails while any is missing |
| `TILO_PACKAGES_TOKEN` | `main` | the bot t-packages' token, which may only write tilo's generic packages: extension manifests, performance results. Performance runs record their results on every reference listed here (`perf-scheduled`, `vm-acceptance-perf`), so adding `stable` records stable's |
| `TILO_SITE_TOKEN` | `main` | a token of the website bot that may push the site repository (`tilo/site`), whose forge takes no identity token from t: `imager-winget-acceptance` |

`sign-apps-*` and `sign-engine-*` take `TAPP_PUBLISHER_IDENTITY` and `RUNTIME_BUNDLE_SIGNING_KEY`,
`submit-apps` and `submit-engine` only `TAPP_PUBLISHER_IDENTITY` (registry entries are signed by the
identity that signed the tapp), and `publish-apps` and `upload-engine` only `REGISTRY_TOKEN` (generic
packages). None of them holds two of those kinds.

### RPM signing

Fedora RPMs were signed by a broker installed on the release runners, which took the job's
Forgejo identity token and returned signed copies. On t the key becomes an RSA-4096 AWS KMS key
(`alias/tilo-rpm-signing`, `SIGN_VERIFY`, `RSASSA_PKCS1_V1_5_SHA_256`) like the other release
keys, and its OpenPGP certificate is made once, checked in beside `keys/cosign.pub` and published
as the repository's `RPM-GPG-KEY`: regenerating it would change its self-signatures. A
`sign-desktop-rpms` check on `tag:desktop-v*` `needs` the Fedora package lane, declares
`id_token = ["sts.amazonaws.com"]` and assumes the release signing role, whose policy then also
names this key. `rpmsign --addsign` runs with `%__gpg_sign_cmd` pointing at a small signer that
hashes locally, asks KMS to sign the digest (`MessageType=DIGEST`) and wraps the answer in an
OpenPGP v4 signature packet (Sequoia's serialization) for the certificate's key id. The check
then verifies each signature with `rpmkeys --checksig` against the checked-in certificate and
that the signed RPMs' immutable headers and payloads equal the tested ones
(`dist/rpm/sign-rpms-broker.py` already compares them). No secret is stored; what remains to
build is that signer.

## Windows

t has no Windows workers, and builds, packages and signs the imager for Windows without one,
from Linux:

| check | what | runs on |
|---|---|---|
| `build/windows-sdk-<arch>` | the MSVC CRT and Windows SDK cargo-xwin uses, at the versions pinned in `tools/ci/windows/xwin-env.sh` | any node |
| `build/windows-<arch>` | `imager.exe` for `<arch>-pc-windows-msvc` with `host-media-writer`, as the release ships it | any node |
| `build/windows-<arch>-tests` | the imager's unit-test executable | any node |
| `package/windows-<arch>` | unsigned `tilo-imager-windows-<arch>.exe`, `.zip` and `tilo-imager-setup-windows-<arch>.exe` (Inno Setup under Wine), with digests | x86_64 |
| `package/windows-winget` | both installers in one WinGet manifest set, validated against WinGet's JSON schemas | any node |
| `wine/windows-x86_64-tests` | the unit tests under Wine, less those `tools/ci/windows/wine-skip.txt` lists with the reason (135 of 185 run) | x86_64 |
| `wine/windows-x86_64-install` | the installer installing silently (files, Start menu entry, the packaged EXE byte for byte), then uninstalling | x86_64 |
| `sign/windows-<arch>` | Authenticode on `tag:imager-v*`: checks the build's digest against the package's, then signs the EXE and the installer with jsign | x86_64, release nodes |

`<arch>` is `x86_64` or `aarch64`. They are `default = false`: run them with
`t test 'build/windows-*'` and so on, or as the needs of the signing checks on a release tag.

**Toolchain.** cargo-xwin drives clang-cl and lld-link against the MSVC CRT and Windows SDK
(the builder image has both MSVC Rust targets, cargo-xwin, and on x86_64 Wine, Inno Setup's
compiler and jsign). Both arches build with clang-cl against the same pinned SDK; for ARM64,
ring compiles with plain `clang` whatever it is told, so `tools/ci/windows/build.sh` puts a
`clang` on PATH that turns clang-cl's include flags into the GNU driver's. Microsoft's license lets anyone download the CRT and SDK for
building but not redistribute them, so they are not in the builder image, which anyone can
pull: `build/windows-sdk-<arch>` downloads them into t's store, which only t's nodes read.
Inno Setup's license allows using and redistributing it, commercially too (its authors ask
commercial users to buy a license, which is a request, not a condition). Fedora builds Wine for
x86 only, so everything that runs Windows programs (packaging both arches, the Wine checks) runs
on x86_64 nodes.

**Signing** needs these secrets, on `tag:imager-v*` and for `release` nodes only:

| name | what |
|---|---|
| `TILO_AUTHENTICODE_CERT_CHAIN` | the code-signing certificate a CA issued for the key below, then its intermediates, PEM |
| `TILO_WINDOWS_PUBLISHER` | the certificate's subject as `keytool -printcert` shows it (`CN=…, O=…, L=…, C=…`); signing refuses a chain whose leaf has another |

The key (`alias/tilo-authenticode-signing`, an `RSA_3072` `SIGN_VERIFY` key in AWS KMS) is not a
secret: `sign/windows-<arch>` names it in `t.toml`, declares `id_token = ["sts.amazonaws.com"]` and
exchanges the token for the role `TiloTPackageSigner`, which trusts only `stable` and the
`desktop-v*` and `imager-v*` tags and may only ask KMS to sign with that key and the RPM key; jsign
signs with the session's credentials.
`TILO_AUTHENTICODE_TIMESTAMP_URL` overrides the timestamp service (DigiCert's by default).
The builder image carries no OpenSSL (`os/tests/no_openssl_runtime.sh`), so the signing check
does not verify the signatures it makes. Windows has to (`signtool verify /pa` and
`Get-AuthenticodeSignature`, as `package-host.ps1` does after signing today), as part of the
smoke test below.
CAs issue code-signing certificates only for keys in a hardware module (FIPS 140-2 level 2 or
better), which a KMS key is; the CSR is made by signing it with the KMS key (for example with
`aws-kms-pkcs11` and OpenSSL), and the CA may ask for proof the key is in KMS. Without a CA,
Azure Trusted Signing issues and holds the certificate, and jsign signs with it the same way
(`--storetype TRUSTEDSIGNING`). This replaces the signer program the Windows runners lease
today, so the key and certificate are new.

**What still needs Windows.** Wine is not Windows, and these stay with a real machine:

- the native unit tests on both arches, above all aarch64 (Wine runs x86 programs only here),
  and the 46 tests `wine-skip.txt` lists: Wine gives no local-disk evidence for its volumes,
  does not protect or inherit NTFS ACLs, and has no privileged writer or identity services;
- running the installed imager (its screenshot), on both arches: its storage publishes only to
  volumes that prove they are local disks, which Wine's do not; and the aarch64 installer's
  smoke test altogether (`test-package-host.ps1`, `test-package-architecture.ps1`);
- verifying the signed EXE and installer (`signtool verify /pa`, the publisher's subject);
- `winget validate`, which also checks what the schemas can't.

The options: keep `release-imager.yml`'s `windows` job for these alone, fed by t's signed
artifacts instead of building its own; or run them in a Windows VM on a KVM node (Windows 11
x64 and ARM64 evaluation images, under QEMU with `/dev/kvm` for x64, and slowly under TCG or on
an arm64 KVM node for ARM64), as a check with `requires = ["/dev/kvm"]` that boots the VM,
copies the artifacts in over SSH and runs the scripts.

**What is not ported.** The release job also takes the desktop and release catalogs from the
`catalog` job and publishes the WinGet pull request (`publish-winget.sh`); those belong to the
imager's release chain on t. Until then these checks run beside `release-imager.yml`'s
`windows` job as a shadow. Once a release tag has produced signed artifacts on t that match the
Windows runners', a prepared cutover (with sign-off) can drop that job's build, test, package and
sign steps, keeping only what still needs Windows (above). The ARM64 development builds are
`windows-arm64-tilo-vm` and `windows-arm64-imager` (dispatch, cross-built).

## What t doesn't do yet

- **Repository variables as channels.** `TILO_APP_COLLECTION_*` and `TILO_DESKTOP_PAYLOAD_*` are
  written by one workflow and read by the next. On t a release check publishes a pointer (a
  receipt in the package registry, or a tag), and the consumer resolves it.
- **Cross-workflow polling.** On Forgejo a consumer polled for another workflow's runs; on t
  the consumer `needs` the producer, which is cached when its fingerprint is published.
- **Inputs from a cargo package's dependencies.** A `[[check]]` lists its input globs by hand;
  the context builds use the workflows' path filters.
- **Service containers and Docker daemons** are gone: images build with buildah, and tests that
  start containers run as micro-VM steps (`isolation = "vm"`), as root in a guest of their own.
- **A Mac for release jobs.** Checks on macOS run on the host, so Developer ID signing needs a
  Mac labelled `release` that takes nothing else.
