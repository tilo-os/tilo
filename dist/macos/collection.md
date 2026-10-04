# macOS SDK app collection

`python3 dist/macos/collection.py` prints a read-only JSON plan. It uses
`tools/apps/app_release.py::app_catalog` and `apps/*/tilo-app.toml`, including
manifest platform defaults and exclusion reasons. The default output is one
`tilo-apps-ARCH.dmg` containing the eligible native SDK `.app` bundles and an
Applications shortcut. `package --per-app` additionally creates one DMG per app.
Nothing installs apps, mounts a completed image, opens Finder, or launches code.

For receipt-backed private macOS 27 bundles with the reviewed media/runtime overlay,
use the separate [staged runtime signer](collection-staged-sign.md). Its signed
layout remains outside this generic SDK adapter's two-executable policy.

At the initial implementation's source revision, the plan contains 45 SDK apps.
Ten manifests exclude macOS: containers, disks, firewall, logs, osk, print-centre,
settings, store, vpn, windows. Browser and Imager support macOS but are separately
excluded here: their CEF/helpers and privileged host writer/assets respectively
require their existing specialized packagers. These are **packager coverage gaps**,
not claims that those apps cannot run on a Mac. See `scripts/browser/package.py`
and `apps/imager/packaging/package-host.sh`. This tool does not fabricate generic
replacements. The Store/runtime `Tilo.app` remains `dist/macos/build-app.sh`;
File Provider integration is not included. A future app is selected from its
manifest, but SDK capability checks and actual builds can still refuse it.

## Prerequisites and commands

Use Python 3.11+, macOS with existing Xcode command-line tools (`codesign`, `lipo`,
`otool`, `hdiutil`), the repository-pinned Rust toolchain/dependencies already
cached, and an existing `tilo-app` CLI built from this checkout. Nothing installs
tools or dependencies. Builds are offline/locked, one Cargo job, using the existing
shared `rustc-budget.py` wrapper's default two-slot namespace, with a continuously
checked 20 GiB disk floor and a 30 minute limit per app. Interrupted builds retain
ordinary Cargo caches for retry. No shared target directory is allowed.

Start with a single app. Use the same `--apps`, `--arch`, `--mode`, source checkout
and `--out` on every stage. Omit `--apps calculator` for the eligible collection;
that explicit build can be large. Replace the two tool paths with existing tools:

```sh
python3 dist/macos/collection.py plan --apps calculator --arch arm64 --mode distribution
python3 dist/macos/collection.py build --apps calculator --arch arm64 --mode distribution \
  --tilo-app /path/to/existing/tilo-app --compiler-guard /path/to/existing/rustc-budget.py \
  --out target/calculator-distribution
security find-identity -v -p codesigning
python3 dist/macos/collection.py sign --apps calculator --arch arm64 --mode distribution \
  --identity 'EXACT_PUBLIC_KEYCHAIN_SHA1_OR_FULL_NAME' --out target/calculator-distribution
python3 dist/macos/collection.py package --apps calculator --arch arm64 --mode distribution \
  --out target/calculator-distribution
python3 dist/macos/collection.py verify --apps calculator --arch arm64 --mode distribution \
  --out target/calculator-distribution
```

`build --reuse-bundles` imports the existing SDK output layout under
`target/tilo-app/macos/PACKAGE/NAME.app`, including its adjacent `NAME.entitlements`.
It validates bundle bytes and manifest identity, but does not establish that an
imported executable was compiled from the current commit. Its receipt says
`existing SDK outputs`. Ordinary `build` delegates to `tilo-app build --target
macos --release`; it does not use the SDK's package command, which can trigger
notarization from environment variables. Compilation requires committed app/build
inputs. The SDK owns plist generation, manifest resources, icons and capability
entitlements. Optional Linux collection feature flags are not forced onto macOS.

Native builds produce the host architecture. `--arch arm64`, `x86_64` and
`universal` validate *every* Mach-O's exact slice set. Universal packaging requires
independently assembled SDK bundles via `--reuse-bundles`; this script does not
install cross targets or pretend host-only bundles are universal. It pins build
`MACOSX_DEPLOYMENT_TARGET=13.0` and rejects code requiring a newer OS than its plist.
Intel execution and real app functionality still need separate qualification.

Build-time dependencies use `CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP=none`.
This avoids [Rust #157750](https://github.com/rust-lang/rust/issues/157750): on
macOS 27, stripped proc-macro dylibs can have a misaligned `LINKEDIT` string pool,
which dyld rejects and rustc reports as `E0463` (cannot find the proc-macro crate).
The app's release stripping and optimization settings remain unchanged. Cargo
may rebuild build-time dependencies once for this profile override; keep the
shared compiler budget and free-space floor. Build receipts record the override;
imported bundles make no claim about their build-time stripping policy.

## Signatures and credentials

Distribution selects **Developer ID Application**, never Apple Development,
Apple Distribution or Developer ID Installer. No matching identity fails; multiple
matching identities require an exact name or public SHA-1. The SHA-1 is a Keychain
reference and certificate pin, not private key material. Verification checks the
Apple anchor, selected leaf, team/authority, every architecture, hardened runtime,
secure timestamp and every nested code object. Signing proceeds helper then app;
`--deep` is used only for verification. The SDK helper keeps its existing empty
entitlement policy; app entitlements come from the SDK sidecar, limited to the
current `tilo-caps` capability keys. No debugging, JIT, unsigned-memory, sandbox or
library-validation exceptions are added. Runtime capture needs separate functional
qualification and must not be assumed from successful signing.

For a local fixture/development package use `--mode development --identity ad-hoc`
on `sign`, optionally `--offline` for explicit timestamp-free operation. Development
also accepts a single Apple Development identity. `--offline` is refused in
distribution mode: secure timestamps require Apple's timestamp service. Signed
and notarized are separate states; all ordinary stages report `notarized: false`.

The script only queries public identity metadata and calls `codesign`. It never
exports or reads private keys, imports certificates, changes ACLs, unlocks a
Keychain, or creates a notary profile. If macOS requires key-use permission, the
operator must resolve it; the script does not bypass prompts. A failed/timed-out
signing step does not publish a stage. Neither raw credentials nor environment
variables are written to receipts.

## Validation, transactions and resume

Stages produce `built/`, `signed/`, `images/` and optionally `notarized/`, each
with `receipt.json`. Receipts contain the source commit, catalog entries and
manifest hashes, architecture, exclusions, public signing facts, file SHA-256s,
symlink destinations, modes and parent receipt hashes. Final DMGs get `.sha256`
files after their last mutation. Secure timestamps and DMG metadata mean these
are reproducible *inputs and traceable outputs*, not byte-for-byte reproducible
signed images. Receipts are local integrity records, not publisher attestations;
Tilo's independent registry/publisher trust is unchanged.

Each stage uses an exclusive output lock and an owned temporary directory, then
renames a completed stage into place. Existing output is never overwritten.
`--resume` accepts only matching plans and unchanged file hashes/options. Failed
stages remove only their own temporary directory; rerun that stage to retry.
A new source commit, app selection or signing identity needs a new output folder.

Validation rejects missing apps/helpers, changed manifest/plist identity or version,
missing declared icons, escaped/broken symlinks, unreviewed nested code, external
Homebrew/build-host libraries and unresolved rpaths. Non-system dylib dependencies
are refused because this adapter covers the statically linked SDK shape. The current SDK bundle shape
is exactly the app executable plus `tilo-runtime-mcp`; custom frameworks, CEF and
extensions require a reviewed specialized adapter. `verify` checks hashes,
architectures, signatures and `hdiutil verify`, without mounting or running apps.
This does not prove GUI behavior, Gatekeeper's first-download UX, or Intel support.
Do **not** substitute `verify-app.sh`: that older runtime-specific verifier launches
a daemon and may create throwaway Keychain items.

## Optional Apple submission (separate authorization)

This stage is disabled unless all three explicit options are present. It uploads
DMG contents to **Apple's notarization service**, which uses Apple/Amazon S3 upload
endpoints and CloudKit for stapling. It is not an App Store or public release.
It requires a working, previously provisioned `notarytool` Keychain profile and
network access. Only run after separately authorizing that upload:

```sh
python3 dist/macos/collection.py notarize --mode distribution --arch arm64 \
  --apps calculator --out target/calculator-distribution \
  --submit-to-apple --notary-profile EXISTING_PROFILE
```

The stage records public submission IDs in `notary-submissions.json` immediately,
waits with a bound, requires `Accepted`, retrieves the log (issues block completion),
staples/validates the DMG, assesses it with `spctl`, then hashes final bytes. A retry
reuses a recorded submission for the same image hash. An uncertain upload without
an ID fails closed: inspect `xcrun notarytool history --keychain-profile NAME` and
reconcile the journal before retrying; it never blindly uploads again. A crash
leaving `notary-submissions.pending` also requires operator inspection. The immutable
manifest inside the DMG describes its pre-submission state; the external notarized
receipt records completion. No submission was made while implementing this script.

## Audit an existing helper before another build

The SDK helper can link to host-installed media libraries even when built with
`--no-default-features`. The collection adapter rejects these dependencies. A
successful Cargo build or a resolved `otool` graph does not establish that the
runtime is portable or ready for signing.

Use this read-only command to gather evidence from existing inputs:

```sh
python3 dist/macos/collection.py audit-runtime \
  --runtime-binary /path/to/qualified-checkout/target/release/tilo-runtime-mcp \
  --runtime-prefix /opt/homebrew/opt/gstreamer \
  --dependency-root /opt/homebrew --arch arm64 --minimum-macos 13.0 \
  > runtime-audit.json
```

The command uses only local `lipo`/`otool` inspection and file reads. It does not
compile, execute inputs, access Keychain, copy libraries, sign, install or download.
It reports JSON to stdout and exits **2** because a GStreamer runtime profile has
not been qualified; this report is evidence, not a successful package receipt.
`--dry-run` has the same read-only inspection behavior for this stage.

The report separates static dependency resolution, runtime-profile completeness,
deployment compatibility and redistribution evidence. Each architecture is
inspected independently, with loader/executable/rpath context, symlink provenance,
hashes and flat-name collision detection. Default bounds are 512 images and a
120-second traversal budget; each image inspection is bounded to ten seconds,
regular image files to 1 GiB and contexts to four times the image bound. Metadata
must resolve within the declared dependency roots and is limited to 2 MiB per
regular file. Directory enumeration is bounded before sorting. An in-progress image
inspection may finish after the traversal budget; any truncated traversal remains
incomplete. `--max-images` and `--audit-seconds` allow bounded adjustments.

All installed GStreamer plugin candidates and the plugin scanner are inspected,
including both main-executable and scanner loader contexts.
That inventory is **not** a required-functionality profile: a broken optional
plugin is not proof that Tilo needs that plugin. Formula metadata provides hints
about dynamically loaded modules (for example, `libsoup`), license labels and
notice locations. Those hints do not establish per-component redistribution
rights, corresponding-source provision or a modification/replacement strategy.
Likewise, an empty list of static errors never establishes complete plugin,
scanner, GIO/TLS, schema, locale or other resource discovery.

An actual relocation adapter must consume a reviewed, pinned functionality profile;
copy compatible inputs into Apple's nested-code locations; rewrite install names
and rpaths on copies; configure relocatable plugin/scanner/resource discovery; and
extend exact code inventory, inside-out signing and verification together. It must
preserve required functionality and include component-specific notices and source
provision. This audit does not implement or authorize that adapter.

Do not lower a library's load-command deployment version or silently raise the
bundle's declared minimum to make validation pass. Existing Homebrew libraries can
require a newer macOS release than the SDK's macOS 13 target. Retaining that target
requires compatible dependency inputs; choosing a newer target requires explicit
approval and qualification. Do not silently remove codecs, promise a source offer
or add a library-validation exception.

Relevant primary guidance:
[GStreamer macOS deployment](https://gstreamer.freedesktop.org/documentation/deploying/mac-osx.html),
[GStreamer licensing advisory](https://gstreamer.freedesktop.org/documentation/application-development/appendix/licensing.html),
[LGPL 2.1](https://www.gnu.org/licenses/lgpl-2.1.html), and
[Apple nested-code locations](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle).

## Tests

```sh
python3 -m unittest discover -s tools/tests -p test_macos_collection.py -v
python3 -m unittest discover -s tools/tests -p test_macos_runtime_audit.py -v
python3 dist/macos/tests/native_smoke.py --compiler-guard /path/to/existing/rustc-budget.py
```

The second command is explicit and macOS-only. It compiles one tiny inert C fixture
under the shared compiler guard, signs copies ad hoc, creates a temporary DMG,
verifies it and proves tampered resources are rejected. It never executes the
fixture or uses a real signing identity. Apple's image service may require execution
outside a restricted filesystem sandbox. Fixture success is not a Tilo app release.

Apple references checked 2026-10-04:
[distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac),
[notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution),
[notarization/stapling workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow),
[nested code and requirements](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

## Private local runtime adapter

For a separately reviewed, pinned macOS 27 helper closure, see
[private runtime staging](local-runtime.md). This produces an unsigned payload;
it does not relax the general collection's macOS 13 or nested-code validation.
