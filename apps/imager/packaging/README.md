# Tilo Imager host packaging

`package-host.sh` builds Linux or macOS on that native host.
`package-host.ps1` builds Windows. Every package receives a SHA-256 sidecar.
Unsigned builds are suitable for CI validation only.

Desktop session availability uses `apps/imager/desktop-catalog.json`, separately
from the installer-media catalog. An explicitly `unreleased` empty Desktop
catalog keeps Desktop installation unavailable; it does not block other Imager
features or invent a release.

For a qualified Desktop release, retain the exact signed RPMs and their native
reports from the `desktop-package-{arch}` checks. The login test of those packages is
`dist/session/vm-e2e/run.py`;
its report and `source.json` record each RPM's size and SHA-256. That basic
session test does not by itself prove return to the prior session, selected
folder import, or source preservation. Review the full native onboarding
journey, including graphical login and identity setup, before asserting all
five `verified_runtime` fields for those exact package bytes.

Commit the reviewed generator manifest as
`apps/imager/packaging/desktop-release-manifest.json` and its nonempty native
report references alongside the generated `apps/imager/desktop-catalog.json`.
Use the existing `build-desktop-catalog.py MANIFEST --output CATALOG`; it measures
the actual local packages and refuses incomplete or mismatched evidence.
Manifest artifact paths are local generation inputs; report paths resolve
relative to the manifest directory and must remain available in the checkout.
Never populate this manifest from package-build or unit-test success alone.

The Imager workflow's common catalog job uses `desktop-catalog-release.py
prepare` to fetch the declared HTTPS release URLs, verify size and digest, and
regenerate the catalog with that same generator. A nonempty catalog without its
reviewed manifest, matching packages and complete digest-bound native report
is refused. Repository credentials are sent only to the Forgejo origin and
removed on redirects to other origins. Preparation emits a catalog digest and
workflow source revision; Linux, Windows and macOS must consume that receipt
and the exact catalog bytes before building or signing Imager. This wiring
neither provisions signing authority nor starts a release. The existing
protected-tag signing approvals and qualification evidence are still required.

Release hosts produce this matrix (architecture is part of every filename):

| Host | Native downloads | Package-manager publication |
|---|---|---|
| Linux x86-64 + ARM64 | signed Flatpak, AppImage, and portable tarball | Homebrew tap formula |
| Windows x86-64 + ARM64 | signed standalone EXE, modern Inno Setup EXE, portable zip | WinGet manifest |
| macOS x86-64 + Apple silicon | signed/notarized DMG and app zip | Homebrew cask |

The protected tag workflow also builds the live installer on native x86-64 and
ARM64 runners, converts the tested qcow2 to `tilo-installer-linux-ARCH.img.zst`,
and publishes a digest-bound catalog. Both installer images and the catalog
receive detached OpenPGP signatures from the independently pinned Linux release
identity, so direct downloads can be authenticated as well as checksum-checked.
The catalog is also embedded into every signed Imager package. Selecting its Tilo
release downloads the compressed image into a per-user cache and verifies both
the transport SHA-256 and expanded-image SHA-256/size before the raw image is
offered to the writer.

Hardware-specific media also carries an explicit `hardware_profile` identity.
The packager validates the selected ID against `os/hardware/profiles/` and its
declared architecture; CPU architecture alone does not establish board support.
Generic media retains `tilo-installer-linux-ARCH.img.zst` and a `generic`
profile. Board media uses `tilo-installer-linux-ARCH-PROFILE.img.zst` and a
separate `catalog-ARCH-PROFILE.json` fragment. Every catalog is
`tilo-installer-catalog-v2.json` and names the hardware profile of each entry.
The catalog requires one generic image for each architecture and rejects
duplicate profile identities.

After the installer has finished resealing and optional PCR signing,
`build-custom-installer.sh` records `INSTALLER.qcow2.hardware.json`. The sidecar
binds the final immutable OCI image ID, validated image-owned profile payload
(including its update origin), and completed qcow2 SHA-256. Package media with
`TILO_HARDWARE_PROFILE=PROFILE`, or `generic` for generic media; a missing or
mismatched sidecar fails before conversion. This is build provenance, separate
from the UKI/rootfs acceptance checks and physical boot validation.

The Imager labels board media with `PROFILE only` in selection and write review.
Selection IDs include architecture, profile and image digest; the download cache
remains addressed by the authenticated expanded-image digest. Required USB
capacity follows the actual expanded image size, not a fixed advertised minimum.

The installation platform contract uses schema 2: `completeSystem.usbCapacity`
names the selected catalog entry's `image_size` in bytes. It replaces schema 1's
fixed `minimumUsbGiB`, which could understate a full installer's requirements.
The packager measures the expanded raw file, and Imager verifies its size and
digest before comparing it with the selected drive's capacity. Compressed
archive bytes never determine the minimum drive size. Update the website's
contract validator and snapshot together before promoting a release carrying
schema 2; `publish-site-release.sh` runs the website checks before committing or
pushing the copied contract. This platform schema is independent of the media
catalog's schema/version.

Board release publication must additionally sign and verify its media and v2
catalog and embed that catalog in signed host packages; the existing generic
release matrix does not automatically publish the extra files.

Every package launches the wizard as the signed-in user. Only after the exact
external device name is typed does it request polkit, macOS administrator, or
Windows UAC approval for a short-lived helper mode. The helper receives a
digest-bound, secret-free request, repeats the device identity and safety
checks, writes and verifies the medium, then exits. Account and disk-encryption
secrets never cross that privilege boundary.

The Flatpak uses the host's root-owned UDisks2 service as its narrow media
helper. After typed confirmation, polkit authorizes one exclusive read/write
descriptor for the reviewed removable block object. The sandbox gets no direct
`/dev` access, host filesystem access, or `org.freedesktop.Flatpak` host-command
escape. The exact same streaming write, signed-source digest, read-back verify,
profile-tail verify, and hot-unplug identity checks run over that descriptor.
`package-flatpak.sh` exports against the pinned 25.08 runtime, smoke-loads the
result in that sandbox as an ABI check, and emits a signed-release
`tilo-imager-linux-{x86_64,aarch64}.flatpak` bundle. Release runners must have
`flatpak-builder` and `org.freedesktop.Sdk//25.08` preinstalled.
The packaging smoke check also runs the bundled `lsblk` without a device grant
and reconciles its sysfs view with host-side UDisks2 Size, ReadOnly, HintSystem,
IdUsage, and MountPoints properties. A missing service/property, mismatched
size, read-only disk, or incomplete partition record rejects that drive.

Native release packages are fail-closed with `TILO_REQUIRE_NATIVE_PACKAGES=1`.
Linux release builders need GnuPG; CI materializes checksum-pinned
`linuxdeploy`, `appimagetool`, and type-2 runtime binaries from its per-arch
cache. A manual release build must provide those tools plus
`TILO_APPIMAGE_RUNTIME`; implicit “latest” runtime downloads fail closed. Windows
builders need Inno Setup 6 and the WinGet client for manifest validation.
macOS uses the platform `hdiutil`.

Linux release validation executes the AppImage once through its extraction
compatibility path and, on every FUSE-capable kernel, again through its normal
embedded runtime. A FUSE-capable runner with a missing or inaccessible
`/dev/fuse` fails instead of silently weakening the check. Flatpak validation
imports the emitted bundle into a private per-job user installation and runs
that installed ref against the system `org.freedesktop.Platform//25.08`
runtime; release runners therefore install both the SDK and Platform runtime
system-wide.

An `imager-v*` release tag runs the signing checks of t.toml, each of which holds
only the t secrets it needs and fails, naming what is missing, when one is not
configured; main's newest head builds and checks the same files unsigned.

- Linux: `sign/linux-{arch}` and `sign/medium-{arch}` sign every tarball,
  AppImage, Flatpak and installer medium with `TILO_LINUX_SIGNING_KEY_ASC` in a
  keyring that exists only for the step, and check it is the key
  `TILO_LINUX_EXPECTED_FINGERPRINT` names. The key's public half,
  `tilo-release-key.asc`, and its fingerprint are published beside the
  downloads, and the publication re-verifies every signature against that
  fingerprint. `release/linux-{arch}` builds the tag's packages with the capture
  service signed by the capture identities (`TILO_CAPTURE_RUNTIME_IDENTITY_KEY`,
  `TILO_CAPTURE_SERVICE_IDENTITY_KEY`, `TILO_CAPTURE_QUALIFIER_BASE`,
  `TILO_CAPTURE_SOURCE_ASSETS`).
- Windows: `sign/windows-{arch}` signs and timestamps every executable with
  `TILO_AUTHENTICODE_CERT_CHAIN` and checks each signer against
  `TILO_WINDOWS_PUBLISHER`.
- macOS: `sign/macos-{arch}` signs the app with the hardened runtime from
  `TILO_MAC_SIGNING_P12`, notarizes and staples the app and the DMG
  (`TILO_APPLE_ID`, `TILO_APPLE_TEAM_ID`, `TILO_APPLE_APP_PASSWORD`), and refuses
  a certificate other than `TILO_MAC_EXPECTED_TEAM_ID` and
  `TILO_MAC_EXPECTED_CERT_SHA256` pin.

Linux users should download `tilo-release-key.asc` and
`tilo-release-key.fingerprint`, compare the complete fingerprint through an
independent Tilo-controlled channel, and then verify a download, for example:

```sh
gpg --import tilo-release-key.asc
gpg --fingerprint "$(tr -d '[:space:]' < tilo-release-key.fingerprint)"
sha256sum --check tilo-imager-linux-x86_64.AppImage.sha256
gpg --verify tilo-imager-linux-x86_64.AppImage.asc \
  tilo-imager-linux-x86_64.AppImage
```

The release attachment is the convenient key transport, not by itself the
trust anchor: the full fingerprint must also be pinned on the Tilo website or
another independently authenticated channel before the first release.

Signed downloads are published to the public GitHub repository
`tilo-os/releases` by `publish-github-release` (`GITHUB_RELEASE_TOKEN`), only
after every download has been installed and started. Homebrew and WinGet follow
it: `publish-homebrew-tap.sh` (`HOMEBREW_TAP_TOKEN`) updates the first-party
`tilo-os/homebrew-tilo` tap, replaces the version, immutable base URL and release
SHA-256 values, and runs strict audits; `publish-winget.sh` (`WINGET_GITHUB_TOKEN`,
`WINGET_GITHUB_USER`) submits the generated three-file manifest to Microsoft's
public community index. See `docs/imager-release-operations.md`.

`TILO_HOMEBREW_TAP_REPOSITORY` and `TILO_HOMEBREW_TAP_NAME` name the tap
the release updates and the tap people are told to run. The tap README, the
native install tests and the landing page's Homebrew commands all read that one
destination, and `docs/install-contract.json` publishes it so the site prints
the tap this release actually advanced instead of a name of its own.
`publish-site-release.sh` refuses to promote a page whose rendered commands
name any other tap, and `scripts/check-install-contract.py` fails when the
contract, the workflow and the formula and cask templates disagree.

The landing-page manifest records the public release root and package-channel
state separately from signed download availability. Homebrew is marked available only after its native
install matrix passes. A newly opened WinGet submission is marked `submitted`,
so the site recommends the signed setup EXE instead of showing a command that
cannot resolve yet. The scheduled acceptance monitor (t's `imager-winget-acceptance`
check) checks Microsoft's exact version manifest every six
hours with a site token from the server config.
Once accepted, it runs the same idempotent site verification/build and changes
WinGet to `available`; transient GitHub failures remain fail-safe as
`submitted`.

Public GitHub releases remain drafts while artifacts are uploaded. Publication reads
every attachment back and compares its exact bytes, then makes the complete
release public in one final API update. A failed upload therefore leaves a
recoverable private draft; an already-public release is immutable and cannot be
filled in or changed by a rerun.

### Versioned installer catalogs

Every native host embeds one `tilo-installer-catalog-v2.json`. The filename
must match the document schema; missing, duplicate, unsupported, or conflicting
catalogs fail before packaging. Schema 2 names the hardware profile of every
medium, so a board image is never mistaken for a generic one.

Each Linux signing job checks the catalog archive size and SHA-256 before
signing every listed medium for its architecture. Promotion authenticates the
selected catalog with the independently expected OpenPGP identity, then verifies
every listed medium against both that catalog and its detached signature. Both
generic architectures remain mandatory. Supporting versioned catalogs does not
create a board image: the corresponding signed OS and medium build must supply
the board artifact and provenance before it can enter the release catalog.

## Native migration service assembly

`package-capture-service.sh --build-only OUTPUT PINS_JSON` compiles the protocol-6
headless worker and signing tools against the unified SDK graph. Assemble those
actual outputs in the same native builder with `--assemble-only`; its pinned
Rust toolchain must be on `PATH`. Assembly also needs a C compiler, `pkg-config`,
GPGME development headers and libraries (`gpgme-devel` on Fedora,
`libgpgme-dev` on Debian/Ubuntu), the configured GnuPG engine, `bubblewrap`,
GNU `tar`, `binutils` and the `ostree` package providing `/usr/bin/ostree`.
The signed capture runtime retains that actual Flatpak repository helper.
`capture-native-preflight.sh` checks the actual native
headers, linkable GPGME library, pinned Rust shared standard library and tools
before the configured OpenPGP engine probe runs. Assembly reuses
`os/scripts/collect-runtime-dependencies.py` to include both transitive native
links and the SDK's observed `.tilo.lazy_needed` first-use libraries. The same
signed-runtime closure verifier used by ordinary app packaging checks the result;
missing libraries stop assembly before any pins are emitted.

Assembly requires explicit `TILO_VERSION`, `TILO_CAPTURE_RUNTIME_IDENTITY` and
`TILO_CAPTURE_SERVICE_IDENTITY`. It never generates signing keys. Its output is
a signed candidate sidecar plus public pins; passing signature, protocol and
closure checks does not qualify source migration or installed consumers.

Select that exact candidate at host build time with `TILO_CAPTURE_PINS` and
`TILO_CAPTURE_SIDECAR`. The build embeds admitted pins in its output directory;
the checked-in default stays `null`. Host and Flatpak packaging require the
actual native release qualifier to pass before shipping a selected sidecar.
A ready flag or a successful worker build cannot replace that qualification.

### Forward compiler cohort evidence

`record-capture-cargo.py` records new Cargo invocations; it has no old-log import
mode. Set `TILO_CAPTURE_BUILD_EVIDENCE` to a private output directory and use its
absolute path as `CARGO` for `package-capture-service.sh --build-only`. This
captures the actual unified worker/runtime Cargo stream and source snapshots
before and after each command. Other roles use the same recorder directly with
their ordinary `build --locked ...` arguments. The native test role uses
`test --locked -p imager --lib --no-run --features native-capture-qualification`.

Record assembly with `record-capture-cargo.py assemble --sidecar FRESH_OUTPUT
--pins FRESH_PINS`. It invokes the canonical producer and preserves its real
status and logs, then checks the signed candidate. Its receipt records artifact
assembly, never migration qualification.

Once every required role has a successful invocation against unchanged current
sources, run `record-capture-cargo.py cohort --evidence PRIVATE_OUTPUT --output
FRESH_COHORT_JSON`. The default requires all fourteen release roles. The explicit
`--source-backend-only` selects the four source backend roles and cannot qualify
publication. Cohort export rechecks current sources and the actual qualifier's
role admission contract. Any source change invalidates earlier role evidence;
finish edits before recording the cohort rather than relabelling old logs.

For the full shared-file edit proof, add `--with-writer` before `--build-only` or
`--assemble-only`. The ordinary Files and Writer executables are then selected in
the same unified thin graph as the capture worker and SDK. Its measured full
role is `writer`; source-only evidence still uses four roles. The receiver must
pack, register and launch `so.tilo.word-processor` through the ordinary signed
app pipeline before invoking its real edit/save actions. A direct root write or
a standalone executable invocation is not that proof.

The normal app collection signs `bin/tilo-runtime-account-capability` into its
SDK runtime. Image builds also preserve that actual graph-built probe, install
the runtime-version marker, and run the installed account consumer verifier only
after the helper, app store, signed runtime and trust files exist. Its generated
receipt records observed protected-grant protocol and exact helper/SDK hashes.
Without the separate optional service-session qualifiers it contains no
`service_sessions` proof.

The receiving tool roles are measured current `tilo-app-package`
(`tilo-app-package-cli`), `tilo-runtime-bundle` (`runtime-bundle`) and
`tilo-install` (`install`). Qualification stages those actual compiled ELFs
read-only at their canonical `/usr/bin/` paths. A native builder image with no
installed tools cannot silently substitute a prior binary or synthetic registry.
