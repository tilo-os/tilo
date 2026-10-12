# Android mediation acceptance slice

This is a private runtime integration and an owned SDK-built test APK. It is not
an installed Tilo app, a package-store publisher, or a general Android framework.

`patch_framework.py` checks exact SHA-256 pins for the eight modified Java sources
from ATL commit `f4622e36186f5757b75a605deff543145df3094b` before changing anything.
Upstream Java sources retain GPL-3.0-only licensing. The added chooser and fixture
sources carry MIT notices. No upstream or built framework/APK binary is vendored.

The patched framework denies unqualified Android permissions, delivers denied
`onRequestPermissionsResult` results on the UI thread, and sends document intents
to the launcher's authenticated chooser FD. The shell chooses only within its
existing file grants; the bridge cannot create grants. Missing service, malformed
reply, oversized frame, stream loss and timeout return cancellation. A transport
failure closes the stream permanently so a late reply cannot satisfy another
request. Requests serialize off the UI thread; a finishing activity gets no reply.
Multiple selection and persisted Android URI permissions are unsupported.

File-URI `ContentResolver.query` supports a null projection, `_display_name`,
`_size` and filename-derived `mime_type`; unknown columns are null. Metadata
requires a successful read-authorized open inside the existing launcher sandbox,
then uses `fstat` on that descriptor. It never reopens a checked path or reads
content to guess MIME type. UTF-16/UTF-8 conversion preserves supplementary-plane
filenames and rejects embedded NUL. Projection and path sizes are bounded.
Precancelled queries fail before access. The two-argument `openOutputStream`
delegates to ATL's existing descriptor mode parsing and confinement.
This does not add `content://` providers or grant authority. Cold restoration
requires the separate real-FUSE journey below; a metadata API result alone is not proof.

Notification APIs check Tilo's existing provider over the already registered
GApplication filtered bus. Explicit fixture preparation with `--notifications`
declares only the existing `notifications = "via-host"` portal; the default declares
none. The entry selects the freedesktop backend, which remains behind the launcher
filter. Actual delivery and withdrawal, with authenticated room attribution, are
qualified. Notification tags, actions and background persistence are not.

Android content descriptions propagate through GTK's existing AT-SPI tree.
TextView labels retain them after text changes; the external client must observe
the expected semantic name. This is representative coverage, not a complete
Android accessibility/resource bridge.

Started services receive unique, increasing request IDs reserved before dispatch
to the main looper. `stopSelfResult` rejects an older request, checks the registered
service instance, removes it once and invokes `onDestroy` on the main looper.
`stopSelf` overloads use the same registry; a bound-only instance cannot self-stop
as a started service. These changes use the existing APK process and private
workspace storage, without an independent service supervisor or new capability.
Legacy foreground workers require the existing filtered notification provider;
their ongoing notice is persistent and bound to the authenticated bus owner.
Normal service stop and owner loss withdraw it. Other bus senders cannot close
or replace the notice. Modern foreground permission remains denied; media and
notification-detach variants explicitly report unsupported behavior. Foreground START_STICKY recovery is bounded to the original launch sandbox
and current authority; intent redelivery is unsupported. Bound-client
lifetimes and independent background execution remain unqualified.

## ARM64 cross-build admission

Signed ARM64 acquisition on an x86_64 builder is staging only: pass `aarch64` as
`acquire_alpine_runtime.sh`'s third argument. Target repository keys come from the
immutable image's existing alpine-keys package; apk uses the explicit target key
directory and disables target package scripts. No host installation or binfmt
registration occurs. Keep a separate signature-verified development sysroot with
musl-dev, gcc, gtk4.0-dev and glib-dev, including headers and pkg-config metadata.
The compiler libraries remain build inputs rather than part of a host install.

Run `build_arm64_entry.sh SYSROOT NEW_ENTRY` with clang/lld on the remote builder.
It statically links ARM64 musl and that sysroot's GCC compiler builtins.
Run `mediation/build_arm64_bridge.py --help` for the signed target sysroot and
existing generic JNI header directory. It uses only target pkg-config search
paths, strict C warnings and a link that rejects undefined symbols. The current
framework DEX is architecture-independent. Stage the new ARM64 entry/bridge and
that same DEX using separate receipts; the unchanged signed fixture can then be
prepared for aarch64. Actual ELF/signature/receipt checks passed, with runtime
acceptance explicitly false. Execution drivers accept `--architecture aarch64` and require a matching native
Linux execution host before launching or admitting the runtime receipt. Native
ARM64 ART journeys, generated schema/cache inputs and physical graphics/input
remain separate qualification steps.

## Build and stage

Run on the approved remote builder in a **disposable rootless Alpine container**
with two CPUs, 2 GiB memory and 256 PIDs. Use the official immutable base recorded
in `docs/android-compatibility.md`, signed Alpine repositories and pinned
`art_standalone-dev=0_git20251009-r2`, plus `openjdk17-jdk`, `build-base`, `python3`, `glib-dev`, `gtk4.0-dev`, `pkgconf`.
Keep the unmodified primary-source archive and the complete package/provenance
record. `build_framework.sh PINNED_SOURCE NEW_OUTPUT` compiles all framework Java
sources and the private JNI chooser library with strict C warnings. It refuses an
existing output and mismatching upstream source. ART's signed `dx` converts DEX.
Compile `atl_entry.c` with `cc -static` inside this Alpine container, or use the
existing glibc entry build inside the Fedora task container.

`stage_runtime.py --help` lists the separately pinned base receipt, framework,
chooser and entry inputs. It creates a new runtime and external complete receipt,
never mutating the base. Feed this runtime into `qualify_apk.py`; do not overwrite
an accepted payload or change its receipt after preparation.

The fixture compiles against the existing official SDK Android 35 `android.jar`.
Stage the byte-exact MIT icon with `python3 prepare-res.py NEW_RESOURCE_DIR`.
Use SDK build-tools 36.1.0 `aapt2 compile --dir NEW_RESOURCE_DIR`, then `aapt2 link --manifest
AndroidManifest.xml -I android.jar`, add the fixture DEX, sign with an ephemeral
**test-only** key, and verify the actual APK with SDK `apksigner`. Keep the key out
of the repository. A fresh build/key changes APK and signer pins: record the actual
ones rather than reusing the acceptance record's digest. The declared PNG icon is
a minimal MIT fixture; ATL's pinned GTK startup crashes on a missing icon.

Prepare the signed fixture with activity
`org/tilo/android/acceptance/AcceptanceActivity`, then run
`tools/android/run_mediation_journey.py --help` for the pinned inputs and private
runtime directory. It launches through the real compositor's `applications`
action, requires a presented Android frame, checks the app's actual private result
files, requires an authenticated chooser refusal in production logs, closes with
the shell action and checks pause/stop/destroy ordering and Landlock evidence.
It tears down only its owned compositor and records exact production binary and
PNG hashes. A failed journey writes no successful acceptance record.

For started-service acceptance, compile `ServiceActivity.java` and
`QueuedServiceActivity.java` with
`ServiceManifest.xml` and the same SDK recipe, then qualify the APK with activity
`org/tilo/android/services/ServiceActivity`. Keep that signed APK unchanged between
the previous and changed runtime. `tools/android/run_service_journey.py` takes the
same pinned inputs as the mediation runner. On the previous runtime,
`--expect-incompatibility` requires actual duplicate zero start IDs, a stale stop
returning true and an absent service-destroy callback; it records no runtime
acceptance. On the changed runtime, the runner requires IDs 1 and 2, rejection of
the stale stop, exactly one main-looper service destruction, ordered activity
pause/stop/destroy, preserved visit data across relaunch, and two isolated
workspace histories. It requires presented frames and mandatory confinement,
creates no grants, and keeps reports/screenshots in `target/` or outside source.

To check cancellation before main-looper dispatch, qualify that APK with activity
`org/tilo/android/services/QueuedServiceActivity` and pass `--queued-starts` to
`run_service_journey.py`. Keep the signed APK unchanged between runtimes.
On the previous framework, `--expect-incompatibility` requires an immediate stop
returning false and both canceled commands reaching `onStartCommand`. The changed
framework must return true, omit those commands, allow a later start before the
queue drains, and allow another restart after stopping the running service.
Repeated stops must return false. The runner checks exact callback order,
main-looper creation/destruction, close/relaunch, private history in two rooms,
presented frames and mandatory confinement. Use the existing foreground journey
on the changed runtime to verify open-descriptor interruption and denied relaunch
after withdrawal of its disposable read/write grants.

Build `ForegroundActivity.java` and `ForegroundManifest.xml` by the same SDK
process. Keep the signed APK unchanged between the baseline and changed runtime.
Prepare it with `--notifications` and activity
`org/tilo/android/foreground/ForegroundActivity`, then run
`tools/android/run_foreground_journey.py --fixture-grants --help`.
`--expect-incompatibility` requires the former missing-notification and live
descriptor-revocation defects, rather than treating them as accepted execution.
The positive path tests ongoing visibility beyond the ordinary toast lifetime,
normal stop, untrusted bus close/replace/category attempts, accepted grant
withdrawal interrupting an open-descriptor worker, no unauthorized sticky restart,
and denied relaunch with preserved private visits. It grants only its own new
source-read and output-write files, revokes them, and stops its owned bus/processes.
Prepare the same APK without `--notifications` for
`--expect-notification-denial`; it must render while refusing foreground admission
and starting no scoped worker. This case adds no grants.


For sticky process-loss acceptance, use that same foreground APK and
`tools/android/run_sticky_journey.py --fixture-grants --help`.
`--expect-sticky-incompatibility` requires actual process loss without recovery
on the previous runtime. The positive journey kills only the exact ART child
through a pidfd, requires a new process and null-Intent main-looper Service start,
checks that the application's Activity and private visit counter were not
restarted, and requires a presented recovery surface and persistent notice.
It kills ART again, revokes the disposable grants during the entrypoint's
backoff, and requires the original authority scope to stop before recovery.
No further writes or service creation may occur; explicit relaunch must deny
metadata/read/write and preserve private visits. Reports and PNGs remain private.

The entrypoint owns a fresh anonymous memfd journal for each original launch.
Only currently running foreground services returning START_STICKY are included.
The recovered framework validates each class against this APK's enabled declared
services and delivers null Intent, rather than calling the app's Activity again.
It uses a visible runtime surface and the app's existing foreground notification.
Normal exit, termination, an empty or malformed journal, missing foreground
status and normal service stop do not recover. Abnormal signal loss retries at
most three times with a two-second backoff inside the same original sandbox.
This adds no background permission or detached host supervisor. Revocation and
expiry kill that original scope, including the entrypoint and later ART children.
Service survival after closing the recovery surface remains unqualified.

Compile `test_sticky_entry.c` with strict C warnings on the remote builder and
run `test_sticky_entry.py HARNESS` to check the production fork/exec supervisor's
normal exit, SIGTERM, missing/malformed journal, successful retry and crash bound.
These native tests supplement the actual ART journey; they do not replace it.
The scoped `android-runtime-supervisor` CI group compiles and runs the same
production harness automatically; no SDK, ART, network or privileges are needed.

For the successful path, prepare `IntegrationActivity` with `--notifications`,
then use `tools/android/run_integration_journey.py --fixture-grants --help`.
The explicit flag authorizes only newly created fixture source-read/output-write
grants. The runner uses an owned bus, selects the real picker controls through
external AT-SPI, checks notification delivery and withdrawal, exact document
bytes, read-only/sibling denials, close lifecycle and external content description,
revokes both grants, then relaunches and requires former URI read/write denials.
It always revokes outstanding fixture grants and stops its owned processes.
See the [PR #2908 evidence attachment](https://code.tilo.so/attachments/9cc177c1-c0ef-4607-8872-0a7b1af8c5e0) for actual results and original PNGs.
Write generated reports under `target/android-acceptance/` or outside the checkout;
the runners reject output in tracked source. Keep generated reports and PNGs private outside Git; commit source and tests only.
Never publish private session profiles or signing keys.

For document metadata and editor modes, compile `DocumentActivity.java` with
`DocumentManifest.xml` into one separately signed APK, then qualify those exact
bytes against both the previous and changed runtimes. Run
`tools/android/run_document_journey.py --fixture-grants --help` for inputs.
`--expect-incompatibility` requires the previous runtime's actual null-projection
`NullPointerException` and missing-overload `NoSuchMethodError`; it never marks that
runtime compatible. The changed runtime must pass Unicode names, byte sizes,
projections, cancellation, input bounds, `wt`/`wa`/`rw`/`rwt` modes and invalid-mode
denial. Source writes and ungranted sibling metadata/reads must fail. After grants
are revoked, relaunch must deny former metadata, reads and writes. Both runs use
the actual production launcher, compositor, ART and external accessibility client.

For a cold recipient, build `fs` and the `android_hydration_fixture` example
remotely, then add `--cold-hydration` to the document journey. Use a short private
runtime parent: the filesystem's authority sockets must fit Linux's Unix socket
path limit. The existing test container must already support actual FUSE mounts;
do not expand device access or substitute a mocked filesystem.

The runner creates fresh synthetic owner/recipient identities and an exact-record
View share. The host seeds only the disposable document. The recipient starts
with no manifest or chunk blobs, verified through the production persistent store
while its filesystem process is stopped. Immediately before the real picker
selection, the file must still be projected. Only the actual APK read may restore
it. After the APK closes, the runner verifies stored bytes, restarts the recipient,
uses the existing safe eviction control, waits for durable completion, verifies actual backend absence, and
restarts again. The same APK must then repeat its successful reads, metadata and
output modes. Revocation tests run with the source restored and resident.

Exact-record mounts use `Shared-<record>` names rather than the host's filename.
The fixture's optional private `expected-document-name.txt` is authored before
launch and independently asserts that name. Without that input, the original
Unicode-name assertion remains mandatory. The cold exact-record namespace has no
sibling file; its denial does not replace the ordinary journey's existing-sibling
isolation test. Keep both journeys. All mounts, grants and processes are owned
fixtures and cleaned up; generated JSON/PNGs remain outside committed source.

For foreground interruption against a received file, use the unchanged signed
foreground fixture with `run_foreground_journey.py --cold-hydration --fixture-grants`.
This also requires the production filesystem binaries and an existing FUSE-capable
test environment. It accepts only the compatible foreground work journey, not a
denial, baseline-incompatibility or sticky-recovery expectation. Backend absence
and projected state precede the actual worker read; local state must follow that
read before the harness reads source content. Accepted fixture-grant withdrawal
must interrupt the sandbox retaining open descriptors, stop writes, remove the
ongoing notice and deny former metadata/read/write access on relaunch. The exact
source bytes and private visit sequence remain mandatory. This qualifies app
grant withdrawal, not withdrawal of the independent exact-record share.

## Acceptance checklist

- [x] Actual unchanged Arity APK calculation, persistence and separate room state.
- [x] Actual SDK-built APK sees storage/camera/microphone/location denial.
- [x] Runtime permission request delivers matching denied callback results.
- [x] Document intent reaches authenticated chooser; no grant yields cancel/no URI.
- [x] Actual approved local fixture document read/write through Android ContentResolver.
- [x] Read-only source write and ungranted sibling access fail with unchanged bytes.
- [x] Grants revoked; relaunch denies read/write to former URIs and chooser cancels.
- [x] Filtered notification delivery/withdrawal reaches Tilo with room attribution.
- [x] External AT-SPI client selects real picker controls and observes Android TextView description.
- [x] Real close invokes pause, stop and destroy; owned session/bus clean up.
- [x] Same signed APK reproduces old metadata/mode failures and passes the changed runtime.
- [x] Unicode metadata, byte size, projections, cancellation and editor modes retain file isolation.
- [x] Relaunch denies metadata as well as reads/writes after fixture grants are revoked.
- [x] Actual APK cold FUSE read, verified backend eviction, recipient restart and APK restoration.
- [x] Cold journey denies revoked metadata/reads/writes with the restored source still resident.
- [ ] Persisted URI authority, already-open FD revocation and native Btrfs projection.
- [ ] Full notification identity/tags, actions and background persistence.
- [ ] Compositor loss, room stop, background services and updates.
- [ ] Real-device GPU/DRM, ARM64, touch/IME and complete Android accessibility/resources.

The checked items are bounded x86-64 software/headless execution evidence.
Remaining items are additional platform implementation and qualification scope.
