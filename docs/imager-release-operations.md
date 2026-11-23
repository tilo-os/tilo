# Tilo Installer release operations

The standalone **Tilo Installer** is released by t. Existing package IDs,
filenames and tag prefixes keep `Imager` for compatibility.

## What every head of main gets

On main's newest head, whenever t has room, every download a person can get is
built, packaged and started on a machine that did not build it:

| Platform | Built and packaged | Installed and started |
|---|---|---|
| Linux x86-64, ARM64 | `package/linux-{arch}`: tarball, AppImage, Flatpak (`tools/ci/linux-imager/package.sh`) | `install/linux-{arch}`: each one in a clean image (`tools/ci/linux-imager/install.sh`) |
| Windows x86-64, ARM64 | `build/windows-{arch}`, `package/windows-{arch}`: portable `.exe`, `.zip`, setup `.exe` | `wine/windows-x86_64-install` |
| macOS Apple silicon, Intel | `package/macos-{arch}`: `.zip`, `.dmg` (Intel cross-built on Apple silicon) | `install/macos-{arch}` (Intel under Rosetta) |

`medium/imager-{arch}` builds the installer medium the downloads write.
`release-assets/imager` gathers all of them into the release a person downloads
and checks it is complete, with every checksum
(`tools/ci/imager-release/assets.sh`). On main that is a dry run: it produces
the files and publishes nothing. A break shows on the day it happens and t
bisects it.

## Publishing a release

Pushing a protected `imager-vMAJOR.MINOR.PATCH` tag runs the same checks on the
tag, then:

1. `sign/linux-{arch}` signs the Linux downloads with the release key and adds
   its public half and fingerprint; `sign/medium-{arch}` signs the installer
   medium with the same key; `sign/windows-{arch}` Authenticode-signs the
   Windows packages; the macOS checks sign and notarize theirs.
2. `release-assets/imager` takes the signed outputs and requires every
   signature.
3. `publish-github-release` creates the immutable GitHub release in
   `tilo-os/releases` (`apps/imager/packaging/publish-github-release.sh`), after
   `verify-linux-release.sh` authenticates the Linux trust bundle. It needs every
   install check above to have passed.
4. `publish-winget`, `publish-homebrew-tap` with the `homebrew-{formula,cask}-{arch}`
   install tests, and `publish-site-downloads` follow it.

A release is the owner's decision: nothing publishes from main.

## Credentials

The publishing checks read their credentials as t secrets, set in the t server
configuration; none lives in the repository or on a build machine:

| Secret | Check | What |
|---|---|---|
| `TILO_LINUX_SIGNING_KEY_ASC`, `TILO_LINUX_EXPECTED_FINGERPRINT` | `sign/linux-{arch}`, `sign/medium-{arch}`, `publish-github-release` | the Linux release key and its full fingerprint |
| `TILO_PACKAGES_TOKEN` | `publish-internal-bundle-{arch}`, `publish-target-policy-{arch}` | `write:package` on `code.tilo.so` for the internal installer and target policy packages |
| `TILO_AUTHENTICODE_CERT_CHAIN`, `TILO_WINDOWS_PUBLISHER` | `sign/windows-{arch}` | with the KMS key t's identity token reaches |
| `GITHUB_RELEASE_TOKEN` | `publish-github-release` | write access to `tilo-os/releases` |
| `WINGET_GITHUB_TOKEN`, `WINGET_GITHUB_USER` | `publish-winget` | the WinGet submission bot and its login |
| `HOMEBREW_TAP_TOKEN` | `publish-homebrew-tap` | `tilo-os/homebrew-tilo` |
| `TILO_SITE_GIT_TOKEN` | `publish-site-downloads` | the site's `imager-release.json` |

## Internal installer bundle

Full Tilo without a USB stick (design spec
`docs/architecture/usb-free-install.md` §3) downloads a
schema-2 internal installer bundle: a manifest plus two raw component images
the Imager writes to staging partitions on the internal disk. Each architecture's
`medium/imager-{arch}` t check derives it from the live disk that has
just been built and checksummed:

1. **Test** (`export` or `publish`): installs `btrfs-progs`/`dosfstools`/
   `qemu-img`, runs `os/tests/internal_bundle_test.py`,
   `os/tests/internal_bundle_catalog.sh` and, as root,
   `os/tests/internal_bundle_export.sh` against loop devices.
2. **Export** (`export` or `publish`): `os/dev/export-internal-bundle.sh`
   runs `os/dev/internal-bundle.py export` through `sudo -n` on
   `os/output/installer/qcow2/disk.qcow2`. It copies the one live UKI, unchanged
   and still signed, to `/EFI/BOOT/BOOT{X64,AA64}.EFI` (the UKI finds the
   staging root through `root=tilo-auto` and the Tilo staging root partition
   type), reserves the handoff files, shrinks
   the Btrfs root to its minimum plus 1 GiB, and writes
   `tilo-internal-installer-ARCH{.json,-esp.img,-root.img}` plus the catalog
   entry `internal-catalog-ARCH.json`. Tag builds record the tag as
   `release_id`; rehearsals record `rehearsal-<sha>`.
3. **Publish** (tag only): t's `publish-internal-bundle-{arch}` check runs
   `publish-internal-bundle.sh`, which uploads components first and the manifest last to the Forgejo generic package
   `https://code.tilo.so/api/packages/OWNER/generic/tilo-internal-installer/TAG/`,
   then downloads each file anonymously and compares digests. Existing files
   are never replaced; a rerun only verifies them.
4. **Catalog** (`publish`, tags only): the job uploads only the manifest and
   its entry (artifact `tilo-internal-catalog-ARCH`). `build-release-catalog.sh`
   checks every entry against the exact manifest bytes beside it, parses the
   manifest with the exporter's own validator, requires the package URL
   above, and with `TILO_REQUIRE_INTERNAL_BUNDLE=1` requires both
   architectures. The entries appear as `CatalogRelease::Internal`
   (`purpose: tilo-internal-installer`, `layout: internal-bundle-v2`). The
   GitHub and Forgejo release publishers skip these catalog inputs.

   A release that wants source-host TPM preparation also publishes a small
   target policy bundle: `target-policy-catalog-<ARCH>.json` plus the exact
   `tilo-target-policy-<ARCH>.tar` it names. The archive contains only
   `tilo-target.efi` and one `predictions/generation-<N>.json` with its
   `.uki.sig`, all signed by the release authority whose public key is embedded
   in the shipped Imager. t's `target-policy/imager-{arch}` check, dispatched
   with `published_os_image` naming the verified multi-architecture release
   index, resolves the exact child of that index, requires that child's
   `org.opencontainers.image.revision` label to equal the release's source
   commit, verifies its release signature and SLSA provenance, and reads only
   `/boot/tilo-target` from it. `apps/imager/packaging/export-target-policy.py`
   checks the PE architecture and both the target-UKI signature and the
   generation prediction against `keys/tpm2-pcr-signing.public.pem`, then writes
   the deterministic archive and fragment. `publish-target-policy.sh` uploads
   the archive first and the fragment last to the `tilo-target-policy` generic
   package, then downloads both anonymously and compares their digests and
   sizes. t's `publish-target-policy-{arch}` check does this for a dispatch
   that names the published OS image and the release; it reads the package
   token from `TILO_PACKAGES_TOKEN` like the internal bundle's publish, and the
   catalog omits the entry until one is published rather than naming a package
   nobody published. `build-release-catalog.sh`
   verifies the fragment and its archive with the same producer before merging
   the `tilo-target-policy` / `target-policy-bundle-v1` entry. Until such an
   entry is published, the source installer honestly defers destination-bound
   TPM protection instead of treating an arbitrary local bundle as a qualified
   artifact. The GitHub and Forgejo release publishers skip both target-policy
   inputs; they are package bytes, not release downloads.

   To inspect a bundle by hand, or to rehearse without release authority:

   ```sh
   apps/imager/packaging/export-target-policy.py export \
     --image registry.tilo.so/tilo/tilo@sha256:<published-child> \
     --arch x86_64 --release-id imager-vX.Y.Z \
     --expected-source <40-hex-commit> --output dist/tilo-target-policy
   apps/imager/packaging/export-target-policy.py verify \
     --bundle dist/tilo-target-policy --release-id imager-vX.Y.Z
   ```

   `export ... --diagnostic` writes only
   `tilo-target-policy-<ARCH>.diagnostic.tar`; it checks the bundle shape but
   neither signatures nor image authority, and it never writes a catalog
   fragment. A rehearsal can therefore inspect diagnostic bytes without
   populating an empty catalog or passing a release check.

Why a Forgejo package on `code.tilo.so` rather than a release attachment:
the Imager accepts internal manifests from that origin only
(`trusted_catalog_url` in `apps/imager/src/release.rs`); each architecture
publishes independently without racing one draft release; the root image
routinely exceeds GitHub's 2 GiB asset limit; and the multi-GiB images never
travel through workflow artifacts. Configure Forgejo accordingly:

- the package owner is public, so anonymous downloads work;
- package blobs are served by Forgejo itself: the Imager refuses cross-origin
  redirects, so object-storage `SERVE_DIRECT` must stay off for packages;
- `[packages] LIMIT_SIZE_GENERIC` (and any reverse-proxy body limit) admits
  the root image, typically several GiB;
- the `internal-bundle` broker returns a 5–15 minute token with
  `write:package` for that owner only, after the usual protected-tag OIDC
  validation.

Publishing a bundle does not enable Full Tilo: builds offer it only when
compiled with `TILO_IMAGER_INTERNAL_INSTALL=1` after the VM round trip
passes. What export proves offline, and what remains for that round trip, is
listed in the header of `os/dev/internal-bundle.py`.

## Kernel buildroot snapshot credentials

The kernel buildroot (`tools/ci/publish/kernel-buildroot.sh`)
is authenticated, signed and pinned like the RPM snapshot and kernel; it
needs no stored secret of its own:

- The builds hold no credentials. Each architecture builds natively and must carry a
  matching `/usr/lib/tilo/kernel-buildroot-arch` marker and base, a recorded package
  list, exactly one signature-checked kernel SRPM and no remaining yum repositories.
- Signing exchanges an OIDC identity token for the KMS signing
  role (`os/dev/configure-cosign-kms.sh`) and signs both digests with a transparency
  log entry; publishing pushes them with a `registry.tilo.so` identity token.
- Pinning takes the published buildroots of a release only when main would build
  the same ones (the inputs digest), verifies both signatures with
  `keys/cosign.pub` and writes `os/kernel-buildroot-snapshot.env` from what the
  release's publish step reported (`kernel-buildroot.sh pins`), committed by hand:
  nothing moves the pins by itself.

Do not configure `COSIGN_PRIVATE_KEY` or `COSIGN_PASSWORD`.

## Package indexes

`publish-homebrew-tap.sh` updates the pre-created `tilo-os/homebrew-tilo`, renders
the formula and cask from release digests, audits them, and pushes `main`, with
the `HOMEBREW_TAP_TOKEN` secret.

WinGet's public consumer path is the Microsoft community index. A self-hosted
source is a separate HTTPS REST service that every user must add as
administrator, so Tilo does not make it the default. `publish-winget.sh` creates
the release bot's GitHub fork when needed and opens the one-version PR expected
by `microsoft/winget-pkgs`, with the dedicated bot's `WINGET_GITHUB_TOKEN` and `WINGET_GITHUB_USER` secrets.

t's `imager-winget-acceptance` check runs every six hours and on dispatch
(`t dispatch imager-winget-acceptance`). It reads the release already published by
the landing page, refuses to select another version, keeps that release's download
root and signing fingerprint, and advances `submitted` to `available` only after
Microsoft's public index serves the exact identifier and version. It pushes the
site with a token of the website bot that may push the site repository: the
server config gives the check it as `TILO_SITE_TOKEN`, and the check fails when
it is missing.

After first publication users install with:

```text
brew tap tilo/tilo https://github.com/tilo-os/homebrew-tilo.git
brew install tilo-imager
winget install Tilo.Imager
```
