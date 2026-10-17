# Native capture release qualification

`qualify-capture-release.py` is the fixed executable gate used by
`stage-capture-sidecar.sh`. It executes tests itself. A signed worker, saved
success JSON, protocol advertisement or old artifact receipt does not qualify a
release. Candidate compilation and signature admission remain separate.

The operator supplies `--cohort` (`TILO_CAPTURE_QUALIFICATION_COHORT`) and an
actual immutable `--image sha256:…` (`TILO_CAPTURE_QUALIFIER_IMAGE`). The image must
exist in the operator's rootless Podman store, match the architecture and provide
Btrfs, fanotify tooling, Python, SQLite, tar and an ordinary non-root NSS account.
Real kernel loop devices and privileged native operations belong to a separate
owned host namespace, never rootful Podman. The
qualifier never downloads an image, builds with Cargo, signs artifacts, changes
caller Home or starts a graphical desktop.

The native image is a current-artifact environment: full mode checks its
installed `/usr/bin/shell`, `/usr/bin/fs`, `/usr/bin/tilo-control`,
`/usr/libexec/tilo-prepared-setup` and `/usr/libexec/tilo-account-credential`
against the measured cohort. An old image may be a base for a current verified
artifact layer, but its historical receipt is not that layer's proof. Do not
bind caller-owned files and call them root-owned installed consumers.

## Compiler lineage input

The closed cohort object has exactly `schema` (integer1), `source_root` (this
checkout), `source_inputs` (relative source path to actual SHA256) and `roles`.
Source-only mode requires `worker`, `runtime`, `host`, `imager_tests` and
`backend_tests`. Native source cases execute the backend lib-test ELF; UI
preparation assertions execute the matching frontend lib-test ELF through
`TILO_IMAGER_FRONTEND_TESTS`. Both are measured compiler roles, not interchangeable
binaries. Full mode
also requires `shell`, `fs`, `control`, `launcher`, `credential_helper`,
`prepared_helper`, `home_seeds`, `prepared_extract`, `files`, `writer`, `browser`,
`app_package`, `runtime_bundle`,
`install` and `installer`. The installed launcher must match both its actual
compiler role and the launcher inside the signed SDK. Worker and SDK bytes must match the actually signed sidecar; the host
must match `--host`. The full fixture must verify launched signed Files and Writer payloads against
their actual compiler-selected roles, using canonical signing/packing/install
with a disposable fixture signer and the exact admitted SDK.

Each role contains exactly these fields:

- `compiled_root`, `path`, `sha256`
- `compiler`, `compiler_sha256`
- `producer`, `producer_sha256`
- `source_before`, `source_before_sha256`
- `source_after`, `source_after_sha256`
- `build_status`, `build_status_sha256`

All referenced objects must be stable regular files with protected modes. The
actual successful compiler stream uniquely selects the correct package,
kind/name/test target and ELF. The source-before/source-after objects are equal
and contain every current source input's actual hash. The producer binds the
role, compiled root, exact command, compiler stream, both source ledgers, actual
terminal status and selected artifact. Build status declares the same actual
command, integer exit0 and unchanged sources. This prevents pairing historical
successful ELFs with unrelated freshly measured sources/status. Never augment a
ledger after compilation or restamp an old producer.

The required source map covers the checkout's actual Rust, Cargo and Fluent
inputs, Imager packaging, OS scripts and installed helper sources, including
untracked candidate source files. The canonical data contract is measured at
`crates/platform/imager-backend/src/imager_model.rs`, alongside the native setup
source tree; a frontend reexport is not a substitute for those bytes.
Capture these *before* each actual role build
and again after, rather than synthesizing them from this documentation. A new
qualifier/source change requires a fresh producer graph (a genuine compiler
cache hit is fine). The script does not mint compiler receipts.

## Split compilation and native execution

`prepare-capture-release.py --build-only` compiles and signs the complete fresh
cohort and Browser/CEF graph, but does not construct the receiving image or run
the gate. It emits only `build-inputs.json`, not a qualification receipt or the
packaging environment. This allows compilation in a dependency container whose
namespace UID0 maps to the rootless operator. Image admission and the canonical
full gate must subsequently run as that actual non-root operator; native kernel
work remains in the separately owned privileged namespaces below.

Disposable keys may live in ignored `target/` storage with mode0600 and explicit
`developer-isolated` signing provenance. They do not authorize production
publishers; their receipts retain `publication_qualified: false`. Existing
compiler receipts cannot be restamped after source changes.

## Execution and resources

The script signature-admits the candidate, verifies immutable image identity,
and stages only public artifacts into an owned directory. The image driver exports and retires a stopped rootless filesystem container,
then the authorized operator creates fresh mount/PID/network namespaces. It
pivots into the exported filesystem, retires the old root, and retains a held
initial-PID-namespace witness. This preserves the real CEF sandbox, rather than
using a chroot or disabling sandboxing. Inside that owned native root, the actor
copies the inputs to root-owned `/owned/candidate`, enters private mount
propagation, and uses the
actual `tools/ci/native-stores.sh` to create disposable Btrfs stores. `/owned`
allows the genuine non-root reviewer's traversal; private state remains0700.
The opt-in native-store helper uses atomic `LOOP_CONFIGURE` with AUTOCLEAR
from the first attachment and holds its descriptor across formatting and mounting.
A root-owned0600 journal records each backing device/inode before attachment in
a private evidence bind, surviving forced container teardown. Normal success
requires strict store unmount/detach and no remaining owned children. After the
case namespace is retired, a separate newly owned privileged no-network
observer namespace reads actual kernel loop status and refuses any association to either
recorded backing device/inode. It also must retire; unavailable observation or
a missing/incomplete journal cannot yield qualification. Default native-store
callers retain their existing behavior; this safety mode is explicit.

Both outer and inner command collectors bound output/deadlines and retain child
PID custody with WNOWAIT until process-group retirement. Diagnostic logs stay
0600 in the private evidence bind, surviving case namespace retirement. The
owned runtime uses the ordinary OS umask022 independently of the signing
operator's private mask; state/evidence directories remain explicitly0700.
Privileged driver imports disable bytecode writes into the operator's staged
inputs, so root-owned cache files cannot prevent their retirement. Public archives and every inventory leaf are hash-verified before admitting
mount coordinates and verified again while staging. Besides the existing input
namespaces, only descendants of `/srv/rcargo/tasks/` are allowed as durable
builder coordinates; sibling prefixes and the source/build stores are refused.
Original candidates, source inputs and signatures are rechecked after
execution. Failed or uncertain cases never issue a successful receipt.

`--source-backend-only` runs the existing genuine signed export/import and
cancellation cases. Its receipt has `scope: source-backend` and
`publication_qualified: false`; it cannot satisfy packaging. The signed export
case's documented limits remain: no installed root-helper CLI/polkit, firstuse,
selected Flatpak receiving or default cloud/Files proof.

Default full mode additionally executes the fixed current-source Python fixture
`os/scripts/installer-receive-fixture/receive.py`, with fixed `--candidate` and
`--evidence` arguments inside the owned namespace. It drives actual installed
firstboot and Shell through the same `tilo-control` resources/actions as users.
The earlier reserved Imager libtest selector is no longer the full-fixture
mechanism and is not claimed to have passed.

Every fixed fixture dependency is measured in the current source map and each
actual compiler source pair, then byte-checked while staging to root-owned
`/owned/candidate`. The closed dependency list is `FIXTURE_FILES` in the
qualifier; missing dependencies refuse full qualification. New fixture modules
must be added there before the next genuine producer builds.

The fixture must create its authenticated prepared capsule through actual
producer/source grants; prove shared imported files can be read and edited,
including an edit through genuine Writer actions reflected in the old source;
prove selected apps' data and actual launch; and prove default System credential
custody/helper, FUSE/Files access and the headless identity lifecycle. Its final
whole stream is one bounded closed completion object containing only integer
`schema: 1`, `status: completed`, and these boolean observations:
`shared_files_readwrite`, `writer_shared_file_edit`, `apps_data_and_launch`,
`protected_cloud_custody`, `cloud_fs_and_files`, `identity_headless_lifecycle`,
`owned_resources_retired`. Each must follow the real assertion it describes;
missing, false, extra or wrongly typed observations refuse qualification.
Intermediate diagnostics stay private. A saved result, synthetic controller,
constructed authority DTO or old receiving scaffold is insufficient.

The receiving fixture is under implementation. Until its real dependencies,
services and assertions are complete and actually pass, full packaging refuses;
source-only qualification cannot replace that proof.

An actual successful invocation writes a fresh private `receipt.json` binding
the candidate/cohort/pins/qualifier/image and executed cases' private log hashes.
No detached receipt is accepted as an input. Native execution and release
publication remain the operator's actions; parser and collector tests do not
constitute native proof.

Installed component publisher pins retain the normal runtime's root-owner and
read-only-filesystem checks. Rootless layer installation uses a read-only build
bind of the exact staged Browser publisher pin, with a shared container SELinux
label on that public context file. CEF is installed through its normal full
runtime package, not pre-copied into an overlay lower layer and then replaced.
Exporting the layer does not
preserve its mount flags, so the owned native driver restores read-only binds
for installed publisher pins and exact retained-runtime records before any
consumer starts. Generated capability facts remain writable in their separate
paths. Public installed directories have mode0755 and generated public pins mode0644,
regardless of the operator's private umask; the outer evidence and signing
identities stay private.
