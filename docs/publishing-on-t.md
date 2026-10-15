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
  (`refs`) and node labels (`nodes`) it may reach. A run of someone's working tree never gets
  any.
- **Release jobs run on fresh machines.** Secrets for releases reach only nodes labelled
  `release`, which take nothing but trusted jobs. Those nodes are `t worker --once` pods under
  Kata: one job per VM. Only workers whose token may offer the `release` label get it, so the
  token the other workers share can't claim release secrets. With `isolated_nodes = ["release"]`
  in the server config, the builds a release needs run there too, so no node that ran someone's
  working tree builds what is signed.

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

## Declared in t.toml

| chain | build (no secrets) | sign (stable) | publish (main, development tag) | release (stable) |
|---|---|---|---|---|
| room service | `image-room-service-context`, `image-room-service` | `sign-room-service` | `publish-room-service`: `dev-<commit>` | `release-room-service`: `sha-<commit>`, signed, receipt |
| web entry | `image-web-entry-context`, `image-web-entry` | `sign-web-entry` | `publish-web-entry`: `dev-<commit>` | `release-web-entry`: `sha-<commit>`, signed, receipt |
| ci-fast, ci-control | `image-ci-fast`, `image-ci-control` (built on the ci-fast just built) | | `publish-ci-fast`: `sha-<commit>` | (CI tooling, not a product) |
| local model catalog | `model-catalog-compiler` | `sign-model-catalog`, `review-speech-catalog` (dispatch) | | `publish-model-catalog` (dispatch): the signed catalog in generic packages |
| cloud promotion | `cloud-drift` (Mondays, reports only) | | | `cloud-promotion` (dispatch): the infra pull request |

`t test image-room-service` builds the image from your tree exactly as a release would, without
publishing anything. The publishing checks run only as trusted runs; `t test publish-room-service`
on a working tree reports them skipped ("needs identity tokens: runs on …") after running their
builds.

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
| `room-service-image.yml` | push main (paths), PR build only, dispatch | `GITHUB_TOKEN` (registry push) | x86_64, Docker buildx | `tilo/room-service:sha-<sha>`, digest receipt | declared above; releases from stable |
| `web-entry.yml` | push main (paths), PR, dispatch | `GITHUB_TOKEN` | x86_64, Docker buildx | `tilo/web-entry:sha-<sha>`, receipt | declared above |
| `release-registry-server.yml` | push main (paths), PR | `GITHUB_TOKEN` | amd64, buildx; Rust compiled inside the Containerfile with BuildKit cache mounts | `tilo/tilo-registry-server:sha-<sha>`, receipt for GitOps | same chain with `oci-build.sh` on the repository as context (buildah supports cache mounts and the `github-assets` build context); the image test needs `docker run` |
| `build-ci-fast-images.yml` | PR, push main (paths), dispatch | `GITHUB_TOKEN` | x86_64, buildx plus the daemon's builder for ci-control | `tilo/ci-fast`, `tilo/ci-control`: `sha-<sha>` | declared above; the smoke run of both images is not ported yet |
| `build-release-tools.yml` | PR, push main (paths) | `GITHUB_TOKEN` | x86_64 and arm64 natively, buildx, `docker run` test | `tilo/tilo-release-tools` index `sha-<sha>`, anonymous pull check | per-arch `image-release-tools-<arch>` with `requires = ["arch=…"]`, one publish check making the index |
| `build-ci-builder.yml` | weekly, push main (paths), dispatch | `COSIGN_KEY_REF`, `AWS_SIGNING_ROLE_ARN` (OIDC), `GITHUB_TOKEN` | x86_64 and arm64 natively, buildx with gha cache, canary jobs that run inside the candidate image | `tilo/tilo-builder:<sha>`, `:<rust>`, `:latest`, signed index | per-arch builds, sign on the release node, publish; the canary jobs become checks whose `image` is the candidate, which t can't express yet (see gaps) |
| `release-relay.yml` | push main and `v*` (paths), PR, dispatch | Cosign via OIDC, `GITHUB_TOKEN` | x86_64, buildx with host network for the compiler cache; waits for `build-rpms.yml` by polling the Forgejo API | `tilo/tilo-relay:<semver>`, signed, on tags only | build `FROM` the RPM snapshot (a `needs` on the snapshot check instead of polling), sign and release on `tag:v*` |
| `build-rpms.yml` | push main (paths), dispatch | Cosign via OIDC, `GITHUB_TOKEN` | x86_64 and arm64, Docker daemon | `tilo/rpms:set-<fingerprint>` signed index with SLSA provenance | build per arch, sign with provenance, publish; published once per fingerprint (cached by inputs) |
| `build-buildroots.yml` | push main (paths), dispatch | Cosign via OIDC, `RELEASE_TOKEN` | per kind and arch, buildx | `tilo/tilo-kernel-buildroot`, and a pull request pinning `os/kernel-buildroot-snapshot.env` | build, sign, publish; the pin pull request needs Git and a push credential (see gaps) |
| `build-kernel.yml` | push main and `v*`, PR, dispatch | Cosign via OIDC, `RELEASE_TOKEN` | `disk-100g` x86_64 and arm64, KVM to boot the kernel and probe Secure Boot | `tilo/tilo-kernel` tags per arch, signed with provenance, pin pull request | build, then a privileged VM test check (`/dev/kvm`), sign, publish |
| `build-legacy-guest.yml` | push main (paths), PR, dispatch | Cosign via OIDC, `RELEASE_TOKEN` | `disk-100g` x86_64, root podman, loop devices, KVM | `tilo/legacy-guest` signed, pin pull request | `privileged = true` on `disk-100g` nodes for the disk image and its boot test |
| `build-env-runtime.yml` | every 4 hours, push main (paths), dispatch | Cosign via OIDC, `GITHUB_TOKEN`, `TAPP_PUBLISHER_IDENTITY` (raw Ed25519 identity) | x86_64 and arm64, Docker daemon | `tilo/env-runtime:set-<fp>`, sealed with the tapp publisher identity, signed | the schedule disappears: the check is cached by inputs, so it publishes once per fingerprint; sealing is a signing check holding the publisher identity |
| `build-env-tools.yml` | every 6 hours, push main (paths), dispatch | as env-runtime | privileged container, loop-mounted ext4 | `tilo/env-tools:<runtime>-<catalog>` signed | `privileged = true` build, sign, publish |
| `build-windows-compat.yml` | dispatch | Cosign via OIDC, `GITHUB_TOKEN` | Docker daemon, `disk-100g` Windows guest, arm64 FEX | `wine-tools`, `wine-paired`, `tilo-fex-*-candidate`, signed FEX payload | dispatch checks; consumers take the candidate from its receipt |

### The OS

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| `build-image.yml` (Release / OS) | `v*` tags; dispatch (release, boards, firmware, app collection, desktop payload); called by `os-dev*.yml` for development builds | Cosign via OIDC; `RUNTIME_BUNDLE_SIGNING_KEY`, `TAPP_PUBLISHER_IDENTITY`, `DEV_APP_PUBLISHER_IDENTITY` (raw keys); `REGISTRY_TOKEN`; the EFI/PCR signer program `TILO_PCR_PREDICTION_SIGNER` on release runners; repository variables for the app collection and desktop payload | x86_64 and arm64 natively, buildx, root podman, loop and FUSE, KVM for the install and boot gates, 100 GiB scratch | development: `tilo-ci`, `tilo-vm` tags and disks; release: `tilo:gen<N>`, `:<semver>`, `:latest`, signed with SBOM, SLSA and freshness attestations, board images | development builds on main (`on = ["main"]`, no secrets but a development push token); releases on `stable` and `tag:v*`: per-arch builds (`privileged`, `/dev/kvm`, `disk-100g`), a signing check for Cosign, and one for EFI/PCR that uses the KMS keys instead of a program on the runner |
| `os-dev.yml`, `os-dev-x86.yml`, `os-dev-arm.yml` | PR with a label, dispatch | none (called workflows get no OIDC) | as above | development images | `t test` of the development build check; no wrapper workflows |
| `dev-disk.yml` | dispatch, also from build-image | `GITHUB_TOKEN`, `REGISTRY_TOKEN` | `disk-100g` both arches, KVM | `tilo-vm` disks and `latest` (from main or `v*` only) | a dispatch check |
| `os-dev-installer.yml`, `dev-disk-boot-diagnostic.yml` | dispatch | none | arm64/`disk-100g`, KVM, loop | artifacts only | dispatch checks with `outputs` (`t outputs`) |
| `extension-manifests.yml` | daily, dispatch from Release / OS | `REGISTRY_TOKEN`; Cosign through a separate role and key for extension manifests (OIDC) | podman with the OS image, both arches | signed manifests in generic packages | `on = ["schedule:40 4 * * *", "dispatch"]`, `cache = false` (it observes the published extensions), signing on release nodes |

### Apps, desktop and models

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| `release-apps.yml` | push main (paths); `apps-v*` tags validate only; dispatch | `TAPP_PUBLISHER_IDENTITY`, `RUNTIME_BUNDLE_SIGNING_KEY` (raw keys), OIDC token as Forgejo API credential, writes repository variables `TILO_APP_COLLECTION_*` | x86_64 and arm64 builds in the builder image, llvmpipe smoke tests | tapps and collections in generic packages, registry entries at apps.tilo.so, the runtime package | build per arch (`dist/apps/package-apps.sh` compile and package with throwaway keys) as ordinary checks; signing with the publisher identity as a separate check; publication from main stays the development collection, the release collection from stable; the repository variables become a published pointer (see gaps) |
| `release-desktop.yml` | PR preflight, weekly, `desktop-v*` tags, dispatch | `REGISTRY_TOKEN`, `RELEASE_TOKEN`; RPM signing through a broker on the runner (OIDC) | distribution containers, a full clone for Nix | packages, a Forgejo release on tags, the OS payload and its repository variables | builds as checks; RPM signing with an OpenPGP key in KMS as a signing check; release on `tag:desktop-v*` |
| `host-session-e2e.yml` | called by release-desktop, dispatch, PR | none | `disk-100g` x86_64, KVM | test result | a check needing the RPM build, `requires = ["/dev/kvm", "disk-100g"]` |
| ~~`release-local-model-catalog.yml`~~ | dispatch on main | `TILO_MODEL_CATALOG_SIGNING_KEY` (raw seed), `REGISTRY_TOKEN` | builder image | signed catalog in generic packages | ported: dispatch checks on stable (above) |
| `release-verifier-bundles.yml` | dispatch | none | x86_64 and arm64 | artifacts | dispatch check with `outputs` |
| `perf-scheduled.yml`, `vm-acceptance.yml` (results) | daily | `REGISTRY_TOKEN` to append results | various | `tilo-perf-results` packages | scheduled checks with `cache = false` and a token that may only write that package |
| ~~`cloud-promotion.yml`~~ | weekly drift report, dispatch | `INFRA_PROMOTION_TOKEN` | Git history | a promotion pull request on the infra repository | ported: `cloud-drift` on a schedule, `cloud-promotion` a dispatch on stable with `git = true`; the pull request is opened through the forge's API, so it pushes nothing itself |
| `check.yml` (`checks` job) | push main | `REGISTRY_TOKEN` as a Git write token | | the CI receipt as a Git note | t's own commit statuses replace the receipt |

### The imager

| workflow | trigger | secrets and variables | runner needs | outputs | on t |
|---|---|---|---|---|---|
| `release-imager.yml` | `imager-v*` tags (signed release); dispatch (unsigned rehearsal) | brokers on the runners, each leased with OIDC: OpenPGP signing for Linux, Authenticode for Windows, Developer ID and notarization for macOS, GitHub App tokens for the public release repository, Homebrew tap and winget, a site token; Cosign; `REGISTRY_TOKEN`; the EFI/PCR signer | Linux x86_64 and arm64 with KVM and a render node, macOS on both arches, Windows on both arches | the GitHub release in `tilo-os/releases`, Homebrew, winget pull request, site downloads, installer media | Linux and macOS jobs as checks (`requires = ["os=macos", …]`), each signing step a separate check on `tag:imager-v*`; brokers become fixed credentials as secrets for release nodes; Windows: below |
| `release-imager-winget.yml` | every 6 hours | site token through the broker | Linux | the site's download state | scheduled check, `cache = false` |
| `release-runtime-macos.yml` | dispatch (disabled) | macOS broker, update key | macOS arm64 | artifacts | a macOS check once enabled |

### Not publishing

These only build or test and become ordinary or `default = false` checks (several already are,
see `t.toml`): `check.yml`, `lint.yml`, `app-sdk.yml`, `browser-auth-e2e.yml`, `ci-filesystem.yml`
(its live cloud tests use credentials for test servers: secrets on `main` without `nodes`),
`ci-legacy-apps.yml`, `ci-nightly.yml`, `ci-render-counters.yml`, `ci-staging-loop.yml`,
`ci-weekly-quality.yml`, `ci-development-signing.yml`, `dev-*.yml`, `inspect-rpm-signing.yml`,
`perf-room-capacity.yml`, `sdk-native-envelope.yml`, `standalone-app.yml`,
`wasm-check-qualification.yml`, `web-size-native-envelope.yml`, `resolve-builder.yml`.

## Identity tokens and secrets

Registry pushes and KMS signing use identity tokens from t (issuer `https://code.tilo.so/t`,
subject `repo:tilo/tilo:ref:refs/heads/<branch>` or `…:ref:refs/tags/<tag>`). Which runs get
them is t's server config; what they may do is the other side's:

| audience | refs | nodes | accepted by |
|---|---|---|---|
| `registry.tilo.so` | `main`, `stable`, `tag:v*` | `release` | registry.tilo.so, as the password of the user `oidc`: tilo/tilo may push under `tilo/` from `main`, `stable` and `v*` tags |
| `sts.amazonaws.com` | `stable`, `tag:v*` | `release` | the AWS role `TiloTReleaseSigner`, for `stable` and `v*` tags of tilo/tilo only: `kms:Sign`, `kms:GetPublicKey` and `kms:DescribeKey` on the three release keys |

The key reference (`COSIGN_KEY_REF`) and the role (`AWS_SIGNING_ROLE_ARN`) are not secret; the
signing checks set them in `t.toml`. What has no OpenID Connect verifier yet stays a secret in
t's server config (sops-encrypted in the infra repository), none in this repository:

| name | refs | nodes | what |
|---|---|---|---|
| `TAPP_PUBLISHER_IDENTITY`, `RUNTIME_BUNDLE_SIGNING_KEY` | `stable`, `tag:*` | `release` | the app and runtime publisher identities, for the app and OS chains once ported |
| `TILO_MODEL_CATALOG_SIGNING_KEY` | `stable` | `release` | the local model catalog's signing seed: `sign-model-catalog`, `review-speech-catalog` |
| `REGISTRY_TOKEN` | `stable` | `release` | a Forgejo token that may write generic packages, which do not take t's identity tokens: `publish-model-catalog` |
| `INFRA_PROMOTION_TOKEN` | `stable` | `release` | may create branches and pull requests on the infra repository: `cloud-promotion` |

## Windows

t has no Windows workers. Only `release-imager.yml`'s `windows` job needs Windows: it builds the
imager for `*-pc-windows-msvc`, runs its tests, packages it with Inno Setup, signs it with
Authenticode and smoke-tests the installed app and `winget validate`. Building already works from
Linux (`dev-windows-arm64.yml` cross-compiles with cargo-xwin); Inno Setup runs under Wine;
Authenticode signing works from Linux with jsign against a KMS or Azure Trusted Signing key. Only
the native tests, the installed-app smoke test and `winget validate` need a real Windows machine.
Until t has one, that part stays on a Windows Forgejo runner, or runs in a Windows VM on a KVM
node.

## What t doesn't do yet

- **Repository variables as channels.** `TILO_APP_COLLECTION_*` and `TILO_DESKTOP_PAYLOAD_*` are
  written by one workflow and read by the next. On t a release check publishes a pointer (a
  receipt in the package registry, or a tag), and the consumer resolves it.
- **Cross-workflow polling.** `await-rpm-snapshot.sh` and Release / OS poll Forgejo for other
  runs; on t the consumer `needs` the producer, which is cached when its fingerprint is
  published.
- **Optional needs.** A release check `needs` its signing check, which only runs on stable, so a
  development and a release publication are separate checks.
- **Inputs from a cargo package's dependencies.** A `[[check]]` lists its input globs by hand;
  the context builds use the workflows' path filters.
- **Running a check in an image built in the same run** (the builder's canary jobs).
- **Pull requests that pin a snapshot** (kernel, buildroots, legacy guest) need Git history and
  a push credential in a check.
- **Service containers and Docker daemons** are gone: images build with buildah inside the
  sandbox, and tests that start containers need `privileged = true`.
- **Compiler cache for isolated builds.** Release builds run in fresh directories; the remote
  sccache the runners use is not wired into t checks.
