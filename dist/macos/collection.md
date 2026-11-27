# macOS app collection

`python3 dist/macos/collection.py` prints a read-only JSON plan. It uses
`tools/apps/app_release.py::app_catalog`, which reads native apps'
`apps/*/tilo-app.toml` and the tilolang packages of `apps/tl`, including each
one's committed catalogue entry (`fleet.json`). An app is carried when it runs on
macOS: a native app whose `package.platforms` lists it, or a tilolang package
whose catalogue entry lists it and whose entrypoint runs on the tilolang runtime
the Mac's `Tilo.app` carries. The default output is one `tilo-apps-ARCH.dmg`
holding the eligible `.app` bundles, a `tapp/` directory of the tilolang
packages, and an Applications shortcut when there are bundles. `package
--per-app` additionally creates one DMG per app. Nothing installs apps, mounts a
completed image, opens Finder, or launches code.

A tilolang package is the package format every Tilo app comes in: a person
installs it through the same installer the Store uses (`tilo app install
FILE.tapp`, or the file opened in the Store), and it runs on the tilolang
runtime bundle `Tilo.app` installs on its first start. The collection builds it
with `tilolang build` and signs it with the publisher key exactly as
`tools/apps/tilolang_packages.sh` does; it invents no second format. Its payload
is a compiled module, so one tapp serves every architecture, and the Keychain
identity signs the image, never the tapp. A package's manifest also names the
Tilo runtime release it needs (`tilo-runtime/<release>`, stamped from the
catalogue); `Tilo.app` carries and installs a runtime package of that release
itself, and a package install — from the Store or from a file — fetches any
other runtime this device lacks from the repositories it can reach. A package
naming a release `Tilo.app` does not run is refused by name rather than
fetching a runtime a Mac cannot execute.

For receipt-backed private macOS 27 bundles with the reviewed media/runtime overlay,
use the separate [staged runtime signer](collection-staged-sign.md). Its signed
layout remains outside this generic SDK adapter's two-executable policy.

The plan names every app it leaves out and why, never dropping one silently. A
native connector such as `discord` runs on a workspace's Linux host; the
tilolang `tl-discord` bridge runs on the bun runtime, whose package is a Linux
binary, so a Mac carries no copy of it. A package the catalogue does not list is
a service rather than an app a person opens. Browser's CEF engine remains a
native **packager coverage gap**, not a claim that it cannot run on a Mac: the
tilolang Browser is carried as its tapp. The installer is not an app;
`apps/imager/packaging/package-host.sh` builds it. The Store/runtime `Tilo.app`
remains `dist/macos/build-app.sh`; File Provider integration is not included.

## Prerequisites and commands

Use Python 3.11+, macOS with existing Xcode command-line tools (`codesign`, `lipo`,
`otool`, `hdiutil`), the repository-pinned Rust toolchain/dependencies already
cached, and an existing `tilolang` command built from this checkout. Nothing installs
tools or dependencies. Builds are offline/locked, one Cargo job, using the existing
shared `rustc-budget.py` wrapper's default two-slot namespace, with a continuously
checked 20 GiB disk floor and a 30 minute limit per app. Interrupted builds retain
ordinary Cargo caches for retry. No shared target directory is allowed.

Start with a single app. Use the same `--apps`, `--arch`, `--mode`, source checkout
and `--out` on every stage. Omit `--apps tl-notes` for the eligible collection.
Replace the tool paths with existing tools:

```sh
python3 dist/macos/collection.py plan --apps tl-notes --arch arm64 --mode distribution
python3 dist/macos/collection.py build --apps tl-notes --arch arm64 --mode distribution \
  --tilolang /path/to/existing/tilolang --publisher-identity /path/to/existing/publisher.json \
  --compiler-guard /path/to/existing/rustc-budget.py --out target/notes-distribution
security find-identity -v -p codesigning
python3 dist/macos/collection.py sign --apps tl-notes --arch arm64 --mode distribution \
  --identity 'EXACT_PUBLIC_KEYCHAIN_SHA1_OR_FULL_NAME' --out target/notes-distribution
python3 dist/macos/collection.py package --apps tl-notes --arch arm64 --mode distribution \
  --out target/notes-distribution
python3 dist/macos/collection.py verify --apps tl-notes --arch arm64 --mode distribution \
  --out target/notes-distribution
```

`--publisher-identity` names an existing publisher key, as `tilolang build
--identity` takes it; without it the tool uses the publisher key it finds on the
machine, and `--locked` refuses a package whose ids lock is not committed. A
native app is built with `tilolang build --target macos --release`, which owns
plist generation, manifest resources, icons and capability entitlements; the
SDK's package command is not used, since it can trigger notarization from
environment variables. Compilation requires committed app/build inputs. Optional
Linux collection feature flags are not forced onto macOS.

`build --reuse-bundles` imports existing outputs instead of compiling: the SDK
layout under `target/tilo-app/macos/PACKAGE/NAME.app` with its adjacent
`NAME.entitlements`, and `target/tilolang/NAME-VERSION.tapp` beside it. It
validates the bytes and the manifest identity, but does not establish that an
imported artifact was built from the current commit. Its receipt says
`existing tapps and SDK outputs`.

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
is exactly the app executable plus `tilo-runtime`; custom frameworks, CEF and
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
  --runtime-binary /path/to/qualified-checkout/target/release/tilo-runtime \
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
