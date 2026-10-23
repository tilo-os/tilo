# Tilo Imager host packaging

`package-host.sh` builds Linux or macOS on that native host.
`package-host.ps1` builds Windows. Every package receives a SHA-256 sidecar.
Unsigned builds are suitable for CI validation only.

Desktop session availability uses `apps/imager/desktop-catalog.json`, separately
from the installer-media catalog. An explicitly `unreleased` empty Desktop
catalog keeps Desktop installation unavailable; it does not block other Imager
features or invent a release.

For a qualified Desktop release, retain the exact signed RPMs and their native
reports from `release-desktop.yml`. The login test of those packages is t's
check `host-session-e2e`, which the desktop payload's publication waits for;
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
Generic media retains `tilo-installer-linux-ARCH.img.zst` and schema-1 catalogs.
Board media uses `tilo-installer-linux-ARCH-PROFILE.img.zst` and a separate
`catalog-ARCH-PROFILE.json` fragment. A catalog containing any board variant is
schema 2, emitted as `tilo-installer-catalog-v2.json`; clients deliberately treat
legacy schema-1 entries without a profile as generic only. The catalog requires
one generic image for each architecture and rejects duplicate profile identities.

After the installer has finished resealing and optional PCR signing,
`build-custom-installer.sh` records `INSTALLER.qcow2.hardware.json`. The sidecar
binds the final immutable OCI image ID, validated image-owned profile payload
(including its update origin), and completed qcow2 SHA-256. Package board media
with `TILO_HARDWARE_PROFILE=PROFILE`; a missing or mismatched sidecar fails before
conversion. This is build provenance, separate from the UKI/rootfs acceptance
checks and physical boot validation. Generic builds from older tooling may omit
the sidecar; when a sidecar exists it is always checked, even for generic media.

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
result in that sandbox as an ABI gate, and emits a signed-release
`tilo-imager-linux-{x86_64,aarch64}.flatpak` bundle. Release runners must have
`flatpak-builder` and `org.freedesktop.Sdk//25.08` preinstalled.
The packaging smoke gate also runs the bundled `lsblk` without a device grant
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
`/dev/fuse` fails instead of silently weakening the gate. Flatpak validation
imports the emitted bundle into a private per-job user installation and runs
that installed ref against the system `org.freedesktop.Platform//25.08`
runtime; release runners therefore install both the SDK and Platform runtime
system-wide.

An `imager-v*` release tag sets `TILO_REQUIRE_SIGNING=1`, so packaging fails
closed unless the platform release credentials are configured:

- Linux steady state: `IMAGER_LINUX_CREDENTIAL_BROKER` names the native OIDC
  credential broker. It installs the public verification
  key and exports `TILO_LINUX_SIGNER` plus `TILO_LINUX_VERIFY_KEY`; every tarball
  and AppImage receives an armored detached signature that is immediately
  verified. The protected repository variable
  `IMAGER_LINUX_SIGNING_FINGERPRINT` must contain that key's full OpenPGP
  fingerprint. Packaging refuses a different broker/imported key, exports the
  exact public key as `tilo-release-key.asc`, and publishes its fingerprint and
  SHA-256 sidecars beside the artifacts. The final publication job imports only
  that exported key into a fresh keyring and re-verifies every Linux signature
  against the protected fingerprint. CI has no private-key import fallback.
- Windows steady state: `IMAGER_WINDOWS_CREDENTIAL_BROKER` configures the
  preinstalled signer wrapper after validating the job's OIDC identity. The
  wrapper receives the artifact path, uses a federated short-lived signing
  lease, and returns only after timestamping. `signtool verify` independently
  checks its result. CI neither imports a PFX nor accepts a direct signer-path
  bypass.
  `IMAGER_WINDOWS_PUBLISHER` pins the complete Authenticode certificate subject;
  packaging checks the signer of every executable against it.
- macOS steady state: `IMAGER_MACOS_CREDENTIAL_BROKER` leases the Developer ID
  signing identity and a notarytool keychain profile. CI has no P12 or Apple
  app-password import fallback. The app is signed with the hardened runtime;
  both the app and signed DMG are notarized and stapled before publication.

`IMAGER_MAC_TEAM_ID` and `IMAGER_MAC_CERT_SHA256` independently pin the
ten-character Developer Team ID and complete SHA-256 leaf-certificate
fingerprint. Packaging extracts the actual app signing certificate and refuses
either mismatch.

All Unix broker programs and signer wrappers must be canonical absolute,
root-owned paths outside the checkout and runner temporary directory; neither
they nor any parent may be group/world writable. A broker creates its output as
a new regular, non-symlink, runner-owned mode-0600 file without an extended ACL.
Linux leases contain exactly one `TILO_LINUX_SIGNER` and one
`TILO_LINUX_VERIFY_KEY`; macOS leases contain exactly one
`TILO_MAC_SIGNING_IDENTITY` and one `TILO_NOTARY_KEYCHAIN_PROFILE`. Publication
token brokers write one non-empty token with no newline. Unknown, duplicate,
empty, control-character, multiline, or malformed output fails the release.
Windows applies the equivalent protected-ACL and non-reparse-point contract.
Temporary notary profiles are removed by an `always()` cleanup step, including
after a failed build.

Native and promotion broker paths are separate protected repository variables
because absolute paths are platform-specific; public-key identities are
protected variables as well. Private signing material and OIDC leases must
never enter the repository, workflow environment files, or ordinary artifacts.

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

Publishing to Homebrew and WinGet is a separate credentialed promotion after
the signed release exists. Signed binaries are published to the dedicated
public GitHub repository `tilo-os/releases`, never to the private Forgejo source
repository. The tag job obtains a target-only GitHub App installation token from
`IMAGER_RELEASE_CREDENTIAL_BROKER`; the source job token is not accepted for
this step, verifies it can see exactly one repository, and revokes it after the
promotion. `IMAGER_PUBLIC_RELEASE_REPOSITORY` can select another pre-created
public `OWNER/REPOSITORY`. `publish-homebrew-tap.sh` updates the first-party
`tilo-os/homebrew-tilo` tap, replaces the version, immutable base URL and
release SHA-256 values, and runs strict audits. `publish-winget.sh` submits the
generated three-file manifest to Microsoft's public community index. See
`docs/imager-release-operations.md` for runner labels, protected variables and
the Forgejo OIDC design for short-lived signing access.

The landing-page manifest records the public release root and package-channel
state separately from signed download availability. Homebrew is marked available only after its native
install matrix passes. A newly opened WinGet submission is marked `submitted`,
so the site recommends the signed setup EXE instead of showing a command that
cannot resolve yet. The scheduled acceptance monitor (t's check
`imager-winget-acceptance`) checks Microsoft's exact version manifest every six
hours with the site token t's server config gives it (`TILO_SITE_TOKEN`).
Once accepted, it runs the same idempotent site verification/build and changes
WinGet to `available`; transient GitHub failures remain fail-safe as
`submitted`.

Public GitHub releases remain drafts while artifacts are uploaded. Publication reads
every attachment back and compares its exact bytes, then makes the complete
release public in one final API update. A failed upload therefore leaves a
recoverable private draft; an already-public release is immutable and cannot be
filled in or changed by a rerun.

### Versioned installer catalogs

Every native host embeds exactly one `tilo-installer-catalog-v1.json` or
`tilo-installer-catalog-v2.json`. The filename must match the document schema;
missing, duplicate, unsupported, or conflicting catalogs fail before packaging.
Schema 1 remains supported for generic media. Schema 2 also identifies media
for a specific hardware profile.

Each Linux signing lane checks the catalog archive size and SHA-256 before
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
