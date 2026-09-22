# Frozen native payload reconstruction

The initial native7489 checkpoint retains17 standalone ARM ELFs. Its delta adds
terminal, tilo-home-peer-control, tilo-launcher, tilo-relay and tilo-relay-admin.
The union is22 of32 required release executables, including browser. These are
not thin application binaries and do not contain a runtime bundle or app store.

With that exact union, remaining release targets are:

| Package | Missing binary |
| --- | --- |
| fs | fs |
| tilo-cli | tilo |
| cli-shim | tilo-cli-shim |
| tilo-crypto | tilo-crypto |
| tilo-user-identity | tilo-device-key-init, tilo-image-trust |
| runtime-bundle | tilo-runtime-bundle |
| tilo-unlock-helper | tilo-unlock-helper |
| windows-vm | tilo-windows-vm-control, tilo-windows-rail |

The first eight binaries remain in the original broad release package/feature
graph, but explicit --bin selection excludes every already-retained executable
and unshipped development binary. The Windows pair preserves its original
separate build graph. Original thin LTO/codegen settings and normal remote
sccache configuration remain required. Dependency compilation/linking may still
be needed; a compiler cache is not proof of zero rebuild work.

After release binaries, the unchanged frozen recipe builds every tapp thin app
and libruntime_dylib together in one graph, verifies ELF hardening, constructs
the runtime bundle, and installs the app collection. Source4583 requires these
phases for a complete payload. It is incorrect to package the saved standalone
browser/settings ELF as a substitute for its thin tapp target.

## Producer contract

`resume-native-payload.py` runs only inside an already-admitted native overlay
worker. It does not dispatch jobs. Supply:

- `--source`: clean, full source4583 checkout, separate from updated controller.
- `--inputs`: exact original native expected-input object.
- `--environment`: original compile driver's prepared build_environment plus
  SOURCE_DATE_EPOCH, pointing to public trust and normal private cache-secret
  mounts. The producer creates no operator signing identity. The existing
  runtime/app-collection phases retain their ordinary temporary build-signing
  behavior and key removal.
- Repeated `--checkpoint RECEIPT SHA256`: independently pinned partial receipts.
- Fresh `--evidence`, numeric reconstruction `--run-id` and `--attempt`.

The producer verifies each immutable registry/config/metadata identity and each
allowlisted executable's hash, size, architecture and existing private-material
scan. Overlapping names must have identical bytes. Only verified regular ELF
files enter a scratch seed image, mounted readonly into the original builder.
They are copied after the original cargo clean. All later output validation,
thin-app, hardening, runtime and app-store instructions remain byte-for-byte
unchanged. The original checkout is never edited.

Initial free scratch remains80GiB with an8GiB running reserve and bounded build
lifetime. No whole native build is launched by tests or recipe preparation.

## Honest provenance and consumer seam

The producer uses purpose `tilo-development-reconstructed-native-builder` and
repository `code.podesta.ai/qos/tilo-development-reconstructed-native-aarch64`.
It does not call the legacy exporter or claim the original recipe ran verbatim.
Reconstruction metadata binds the original source/input/recipe, derived recipe
and driver hashes, controller/helper hashes, complete checkpoint receipts,
reused inventory, missing-target selection and reconstruction run identity.

Only after the complete original inventory check succeeds, retained ELF bytes
are rechecked unchanged, projection is revalidated, and immutable registry
readback agrees does it emit `reconstructed-native.json`. The partial receipts
remain explicitly incomplete. The complete reconstructed receipt still says
installable=false and consumer_adoption_required=true.

`native_resume.import_reconstructed` is a distinct opt-in consumer function. It
requires the exact receipt SHA256 and independently approved reconstruction
SHA256, checks immutable publication, and rechecks the complete mounted inventory.
It returns an immutable local image ID suitable for the existing builder-context
override. The legacy source4583 importer intentionally rejects this new purpose.

**Remaining integration prerequisite:** a reviewed assembly controller must call
this explicit importer, preserve frozen4583 source/native expected inputs, then
pass the resulting ID through `TILO_NATIVE_BUILDER_IMAGE_ID`; it must not replace
or fabricate the legacy native receipt. A current-main workflow cannot silently
change the application source to its own SHA. No dispatch is ready until that
controller path, immutable checkpoint admission and worker environment are
reviewed together.

Verification currently covers exact frozen recipe transformation and fail-closed
target selection. No reconstructed image has been built or executed by this
change; remote build and complete-inventory/consumer acceptance remain runtime
gates, not claims from unit tests.

## Manual CI controller

`development-native-resume.yml` uses the existing ARM disk100g lane. Its reviewed
`helpers_revision` must be the coherent install-prep helper branch containing
the producer, compile-driver resume hook/source separation, and bounded public
export helper. It is deliberately independent of the fixed4583 source checkout.
Do not substitute a latest-main helper revision with a different crypto CLI.

Inputs are the exact two public checkpoint receipt strings and their SHA256s,
plus the original WPE adoption receipt/hash. `launch-native-resume.py` rejects a
different checkpoint reference before creating Docker resources. The original
compile driver prepares cache access, native input identity and build environment;
its resume hook invokes the derived producer and returns before the old exporter.

The tools image follows the existing development-compile convention: pinned
568fca1f builder, tools-only package install, and existing pinned cosign. Its
actual configuration digest is recorded in worker.json. This is not an OS or
application rebuild. No reusable published tools image is currently assumed.

The inner worker is capped at4 CPUs and9GiB RAM, within the existing lane's
allocation. Compiler jobs remain2 to preserve the original profile. Live build
logs stream through Docker attach; early cgroup limits, throttling, memory and
available pressure files are sampled at approximately2,60,300seconds. Missing
pressure support is recorded as unavailable. No resource resize is automatic.

Only allowlisted, bounded public JSON evidence is exported, including the
reconstruction plan even on a partial failure. Exact owned Docker resources are
removed on exit/cancellation. No full target directory is promised to survive.
Success requires the complete reconstructed immutable publication receipt;
there is no automatic retry or fallback to the old full native build.

Native reconstruction does not consume a kernel artifact. Pin selection omits
the kernel entirely in this mode. The reproducibly derived shell driver uses
`scratch` for its unused kernel stage, without reading a kernel pin or inventing
an artifact digest. Full-image kernel selection and validation remain intact.
