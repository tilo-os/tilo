# Android applications on Tilo: execution and remaining gaps

Status at 2026-10-08; native ARM64 qualification used published main `e40be46f15fe`. **An unchanged Android APK executes
through Tilo's production compositor and launcher.** Arity 1.1 rendered
`123+456 = 579`, saved its state on tab close, restored it on relaunch, and
rendered `234+567 = 801` with separate state in a second workspace. This is
x86-64 execution on the existing remote Fedora builder with a headless Tilo
compositor and software graphics. Native ARM64 ART additionally passes the owned
service, foreground, notification-denial, sticky-recovery, permission and document fixtures,
including actual FUSE cold restoration and foreground descriptor interruption,
described below. These are headless runtime journeys, not real-device
qualification or general Android application support.
[Android](android.md) describes the opposite direction: building Tilo apps for Android.

## Reference and runtime

The bookmark reference **Vinix** is confirmed as [vlang/vinix](https://github.com/vlang/vinix):
the bookmarked post <https://x.com/v_language/status/2106302325950488863>
links [vinix-os.org](https://vinix-os.org/), which links that repository
(checked 2026-10-07; see issue #3476). Its [Android documentation](https://github.com/vlang/vinix/blob/master/docs/android.md)
and [build inputs](https://github.com/vlang/vinix/blob/master/build-support/android/build.py)
identify ATL/ART and the original Arity artifact. Vinix's reported ARM64 results
and kernel adaptations do not establish Tilo compatibility.

[Android Translation Layer](https://gitlab.com/android_translation_layer/android_translation_layer)
implements part of Android's framework over GTK and Linux libraries. Its
[architecture](https://gitlab.com/android_translation_layer/android_translation_layer/-/blob/master/doc/Architecture.md)
retains DEX and architecture-matched native libraries. Missing framework methods
are compatibility gaps. ATL supplies no Tilo security boundary: the existing
launcher confines each process and its per-room storage.

The tested private runtime uses signed Alpine edge packages:

| Input | Recorded version or pin |
| --- | --- |
| Official Alpine base | `docker.io/library/alpine@sha256:020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000` |
| ATL | `0_git20260705-r1`, upstream `f4622e36186f5757b75a605deff543145df3094b` |
| ART | `0_git20251009-r2`, upstream `e78bf68917bcaaf58fef3960cd88793b3b7f39cc` |
| Acquired rootfs SHA-256 | `1c5d04f37be46e40ad80efbd01f6e982cc85512bb92f3977c5af673e3c064423` |
| Original runtime receipt SHA-256 | `804b2c280485ee16b8051a45b43c4a3389260d90ebed78188b9f0dc0b34908ea` |
| Mediation runtime receipt SHA-256 | `b139102cb605c839ad4d152c0ee7fd364eb55208000d56b6dd9fcd77473ef090` |
| APK SHA-256 | `1928e65ced8cbe78be2ff3cb4c321e9e75d138e8e30ea1368fbb772a86827d1d` |
| APK signer certificate SHA-256 | `830e955c73d02aa077c270fcb3e089a556976afe8e9ab3f9b39c4616fdbd9a6d` |

APK provenance is the [original Google Code archive](https://storage.googleapis.com/google-code-archive-source/v2/code.google.com/arity-calculator/source-archive.zip),
member `arity-calculator/rel/Arity-1.1.apk`. The archive SHA-256 is
`1b39e4f968e7d21602ea166f73a1b36c35213b78e9e3e06b74053cfdc98dfbe2`.
SDK build-tools 36.1.0 `apksigner` verified the real v1 signature and certificate;
`aapt2` identified `arity.calculator`, activity `calculator.Calculator`, minimum
SDK 3 and legacy storage/phone-state requests. No corresponding host grants were
made. The APK has no JNI libraries. A pure-DEX result says nothing about
architecture translation or modern service-heavy applications.

The acquisition ran in a bounded rootless container on the approved builder.
Java 25 for SDK signature verification and Tesseract for visual assertions were
added only to the disposable remote task container. Their package transactions
reported an unavailable man-db scriptlet after installing the binaries; actual
signature verification and OCR execution were checked. Nothing was installed on
the laptop or Tilo OS image, and no device was rebooted.

## Implemented path

`tools/android/acquire_alpine_runtime.sh` repeats signed package acquisition in a
bounded rootless container and exports provenance. Its repositories can change;
record the actual export digest and complete package database each time. The
recorded execution used the equivalent acquisition steps before this reusable
script was extracted. The script refuses a mutable image name, rootful execution
and existing output; its cleanup removes only its own container.

`tools/android/stage_alpine_runtime.py` materializes the private closure from a
checksum-pinned export. Archive links resolve inside that archive, never against
the host. Traversal, duplicate members, cycles, missing targets, special files and
oversized exports fail. Privilege bits are stripped and the result contains only
regular files/directories. An external receipt covers every runtime file.

`tools/android/atl_entry.c` is the native private entrypoint. It finds its runtime
relative to itself, executes the bundled musl loader, sets private ART/JNI,
framework, ICU, font and GTK paths, disables ATL client window decorations, and
uses software graphics and ART interpreter flags. GTK stays inside the payload;
`os/scripts/verify-no-ui-toolkits.sh` and the base OS remain unchanged. The helper
uses launcher-authored `TILO_PACKAGE_ID` for its Wayland application identity.

Pinned ART assumed `getpwuid()` succeeded and aborted in the previous sandbox.
The launcher now supplies a synthetic **read-only** `/etc/passwd` via an owned
memfd, containing only the launch identity, home `/run/tilo/app` and a nologin
shell. It never mounts or discloses the host account database. Descriptor 42 is
reserved alongside the existing seccomp and service descriptors. All three
production spawn paths use it. All descriptor sources are relocated together
before assigning child slots, so a service source on 42 cannot be overwritten;
relocation failure refuses the launch. Landlock grants the generated file read-only.
Bubblewrap, strict `tilo-default`, network isolation and mandatory Landlock stay
in force. No JIT entitlement, extra host filesystem bind or service grant is used.

`tools/android/qualify_apk.py` bounds and checks the standalone APK archive, verifies
exactly one pinned signing certificate, verifies every runtime byte against an
external pinned receipt, and prepares `tilo.toml`, `activity.txt` and
`qualification.json`. It rejects incompatible JNI ABI; split APK delivery and
binary-manifest API admission remain outside this prototype. The content-derived
package identity isolates revisions; update continuity is not implemented.
Preparation is offline and establishes eligibility, not execution.

**Desktop launch uses the existing `applications` semantic action.** The compositor
registers the launcher process and authenticates Wayland connections through
`SO_PEERCRED` and its live process registry. A supplied workspace id or socket
path alone does not admit a client to a room. The old raw workspace proxy is not
the production authentication boundary. Optional `launcher_argv` is only a
standalone confinement preflight; GUI `run` does not forward arbitrary trailing
arguments. The prepared entrypoint reads its activity file itself.

The acceptance catalogue is a private developer fixture. It does not establish
signed Tilo package-store admission or authorize installing a runtime into the OS.
No runtime, APK, container export or upstream library binary is vendored here.
The export preserves package licensing/provenance metadata. ATL is GPL-3.0-only,
ART Apache-2.0, and Arity's archive/Vinix identify Apache-2.0. A distributed binary
bundle still needs complete notices, corresponding-source obligations and package
publisher review; this private execution is not distribution approval.

## Repeat the acceptance journey

Use the existing approved remote builder and a Cargo drop-in with local fallback
disabled for Rust builds. Never share a worktree target directory or build locally for this task.
The C entrypoint can be compiled inside the disposable remote task container:

```sh
cc -std=c11 -O2 -Wall -Wextra -Werror tools/android/atl_entry.c -o /path/atl-entry
python3 tools/android/stage_alpine_runtime.py \
  --rootfs /path/rootfs.tar --rootfs-sha256 '<reviewed export SHA-256>' \
  --entry /path/atl-entry --entry-sha256 '<compiled entry SHA-256>' \
  --architecture x86_64 --provenance /path/provenance.txt \
  --output /path/new-runtime --receipt /path/new-receipt.json
python3 tools/android/qualify_apk.py /path/Arity-1.1.apk \
  --apk-sha256 1928e65ced8cbe78be2ff3cb4c321e9e75d138e8e30ea1368fbb772a86827d1d \
  --architecture x86_64 --activity calculator/Calculator \
  --apksigner /path/sdk/build-tools/36.1.0/apksigner \
  --signer-sha256 830e955c73d02aa077c270fcb3e089a556976afe8e9ab3f9b39c4616fdbd9a6d \
  --runtime-root /path/new-runtime --runtime-receipt /path/new-receipt.json \
  --runtime-receipt-sha256 '<reviewed receipt SHA-256>' --output /path/new-payload
python3 tools/android/run_arity_journey.py \
  --payload /path/new-payload --runtime-receipt /path/new-receipt.json \
  --runtime-receipt-sha256 '<same reviewed receipt SHA-256>' \
  --runtime-parent '<existing user-owned directory with mode 0700>' \
  --output /path/new-acceptance-output
```

The runner requires prebuilt `shell` and `tilo-launcher`, Tesseract/Leptonica and
its English model. It creates a fresh scratch profile and private control socket,
uses the existing `spawn-headless.sh` and `_control.py`, waits for application
catalogue readiness and actual presented foreign frames, and launches through the
semantic application action. Input clicks the actual expression field, not blank
window space. Bounded OCR reads answer and expression independently from the
actual PNG, adding only analysis padding/scaling; original PNGs are retained.

It requires `579`, close/reopen persistence, initially separate second-room state,
`801`, separate saved history bytes and the original `579` on returning to room
one. The separate native entrypoint probe must fail to read or write a first-room
canary from room two and receive a policy denial for TCP connect, while resolving
only the synthetic account. This is a launcher boundary probe, **not** proof of
Android runtime-permission APIs or malicious JNI containment. The runner stops its
own compositor process group, checks control socket cleanup and writes
`journey.json` only after success.

Run-specific receipts, original PNGs, OCR text and confinement results are
preserved in the [execution-evidence attachment on PR #2908](https://code.tilo.so/attachments/9cc177c1-c0ef-4607-8872-0a7b1af8c5e0).
They are generated artifacts rather than repository inputs. Future runs must use
an output directory under `target/android-acceptance/` or outside the checkout;
the runners reject output inside tracked source. Keep full private profiles and
identity directories out of published artifacts.

## Bounded Android mediation slice

The [mediation implementation and acceptance checklist](../tools/android/mediation/README.md)
adds a private, pinned framework variant. Its five upstream Java inputs are checked
against exact hashes before patching. Android permission checks now deny unqualified
storage, camera, microphone and location access; `requestPermissions` returns matching
denied results through the actual UI-thread callback instead of hanging the app.
There are no new host grants or consent bypasses.

Android document intents use a private JNI bridge to the existing
`TILO_SVC_CHOOSER_FD`. The launcher has already authenticated the app/room before
handing over that connection. Requests serialize off the UI thread, use bounded
newline JSON and a two-minute deadline, and never fall back to host filesystem or
D-Bus access. The shell returns only paths covered by its existing grants. Invalid
or lost transport is permanently unusable; malformed/mismatching replies cannot
satisfy a later request. A finishing activity receives no late result. Multiple
selection and persisted Android URI grants remain unimplemented.

Notifications use GIO's freedesktop backend through the launcher's existing
filtered bus. The explicit fixture-only `--notifications` preparation option
adds only the existing `notifications = "via-host"` portal declaration. Android
checks the already registered application connection for Tilo's provider, without
opening another bus or activating a daemon. Without this capability, notification
APIs report unavailable. With it, actual delivery and withdrawal reach Tilo's
notification centre and the provider attributes the caller to its authenticated room.
Notification tags, actions and background persistence remain unqualified.

The owned SDK-built acceptance APK tests the real Android APIs inside ART. It
requests no Tilo file, network or bus grant. It requires denied permission callbacks,
an authenticated chooser refusal producing `RESULT_CANCELED` without a URI, a
presented APK frame, unavailable notification capability, and actual close callbacks
in order: pause, stop, destroy. The runner also requires fully enforced Landlock,
no-new-privileges and owned compositor/socket cleanup. This is actual APK execution,
not a JVM mock or a substitute native boundary probe.

The fixture sources, manifest and minimal MIT icon are in `tools/android/mediation`.
Build against the existing official Android 35 SDK and verify the test-only signed
APK with SDK `apksigner`; no key or APK binary is committed. A new signing key means
new artifact pins. The private framework and transport were built in a bounded
rootless official Alpine container with signed ART development/JDK packages.
Nothing was added to the laptop or Tilo OS image.

The preserved Android API and Arity runs use the same runtime and production
binaries as the successful integration below. Both were rerun after the security fix.

## Successful document, notification and accessibility integration

The [PR evidence attachment](https://code.tilo.so/attachments/9cc177c1-c0ef-4607-8872-0a7b1af8c5e0) contains the integration receipt,
Android callback results, external accessibility tree and three original PNGs.
The runner creates only new disposable source, destination and sibling-canary
documents in its own profile.
Explicit `--fixture-grants` authorizes only the source read grant and destination
write grant; it never accepts a user document path or touches the user's bus.
Android `OPEN_DOCUMENT` selects and reads the exact source bytes through Tilo's
existing chooser. `CREATE_DOCUMENT` and the picker overwrite confirmation save the
exact destination bytes through Android `ContentResolver`. Source writes and sibling
reads fail; blanket Android storage permissions remain denied. Both grants are
revoked before relaunch; the APK then fails both reads and writes to the former
URIs and receives chooser cancellation. This proves revocation on relaunch, not
revocation of already-open descriptors, persisted URI grants or cold P2P hydration.

The first real run found source corruption: automatic app workspace authorship
matched external filesystem resources and upgraded a nominated read-only bind to
write. App and agent `Scope::Workspace` grants now authorize only room resources.
Unrestricted local-user grants retain their existing semantics, and workspace
restrictions still clamp filesystem writes for read-only participants. Production-decider and launcher tests preserve
workspace authorship and explicit file reads while rejecting external writes,
siblings, devices and network authority. The complete APK run now confirms the
read-only source and canary remain byte-for-byte unchanged.

The picker is driven through the existing external AT-SPI client, activating the
real file entry, Save and Replace actions. The shared client now accepts valid GTK
app-namespaced object paths as well as AccessKit paths. A pinned ATL bridge forwards
Android content descriptions to GTK's accessible properties, including TextView's
actual label and preservation after text updates. The external process observes
`Approved Android document status`; setting an internal Java string alone does not
satisfy the runner. This is representative TextView accessibility, not a complete
Android semantic resource/action or system-owned CRDT bridge. Actual close invokes
pause, stop and destroy; the owned session and bus stop on completion or failure.

Final framework inputs: upstream archive SHA-256
`be071b16a9b26f8b5feb0f858d2a850947d0e42a19642ad16a04343cf6be1137`,
patched framework DEX JAR `fe5374cccd21ebb27625121274389914f82c4c1a71cf3406bd72e21e6fd77273`,
private JNI library `8197a4231a283c3bc5ea6b24f9e210faa3fa31b0df66f0dfb01c70a7cc29dfec`,
static entrypoint `210e786537dfbed45e669a5f3cc99c99dd89d80d81cf2192d323d5890e69327a`.
Fixture APK `dedf9add2eebf9cb0b440cb092c346727d9435e874beaa7099ae9ec0b043cab4`
was verified with signer `4b6b7db707642374eb5f4f5bb32634240ce2a0db9120d9a330ce41e63fe963a5`.
The test key stays only in the disposable remote build container and is destroyed
with that container. No key, runtime or APK binary is committed.

Validation includes 21 Python admission/archive tests, 21 remote external AT-SPI
client tests, all 82 capability-model tests and 132 launcher unit tests, mandatory remote
shell/launcher test-target compilation, strict remote lint, complete framework and
strict C builds, SDK signature verification, all three actual APK journeys and
visual inspection of the owned fixture PNGs. All Rust builds/tests run remotely
with local fallback disabled.

## Document metadata and editor modes

Ordinary file clients exposed two framework gaps: `ContentResolver.query` crashed
with a null projection, and `openOutputStream(Uri, String)` was absent. One signed
SDK fixture reproduces both failures in actual ART on the previous accepted
runtime, then passes on the changed runtime with its APK bytes unchanged.

The private integration supports file-URI display names, byte sizes, explicit
projections, unknown-column nulls, precancelled queries and bounded inputs. An
actual `android-π-📄.txt` fixture preserves its supplementary-plane filename.
Editor modes `wt`, `wa`, `rw` and `rwt` use the existing descriptor implementation;
invalid modes fail without changing bytes. The runner verifies exact output,
read-only source protection, denied sibling metadata/reads, close callbacks and
denied former metadata/reads/writes after grant revocation and relaunch.

Metadata requires a read-authorized open inside the unchanged launcher sandbox,
then observes that descriptor with `fstat`; a swapped path cannot redirect the
metadata lookup. MIME is inferred from the caller-supplied filename without reading
content. This remains a file-URI path: Android content providers,
persisted URI authority remain unqualified. The separate foreground-worker journey
below covers interruption of already-open descriptors. The [document journey source](../tools/android/run_document_journey.py)
records actual runtime, APK and production binary pins outside tracked source.
The original signed integration, denial and Arity APKs are rerun against the new
runtime to preserve their prior behavior.

## Cold filesystem restoration

The document APK also reads through independent production FUSE host/recipient
processes on the remote builder. The recipient's persistent store initially has
neither the manifest nor its chunk. Immediately before the real picker selection,
the file is still projected. Actual APK reads restore both blobs with exact bytes.
After close, the existing safe eviction control waits for durable completion;
independent stopped-store inspection requires both blobs to be `NotFound`.
The recipient restarts, and the same signed APK repeats its successful read,
metadata and editor-mode assertions, restoring both blobs again. Revocation
denials then run with the source still resident, rather than relying on missing
data to produce a refusal. All owned mounts, processes and grants are cleaned up.

This exposed and fixed a production chooser bug: Save used the first granted
folder even when that folder was read-only. Open uses read-authorized roots and
Save uses write-authorized roots; the chosen path still needs final authorization.
A service regression test covers separate input/output folders without a save hint.

Repeat with `run_document_journey.py --cold-hydration` and the production
`fs`/`android_hydration_fixture` binaries, following the
[fixture instructions](../tools/android/mediation/README.md).
The exact-record share has a generated filename and no sibling; an independently
authored fixture input asserts its name. The original Unicode/existing-sibling
journey remains separate. The accepted cold APK SHA-256 is
`90965424611eb98666fcc06dfc01f1ea7f3b11221f71f41cdce0b5cf1a0be8a2`,
with signer `05331ce2f12843658fcea5d4d69c214647abee538bfe8c8dc1cc02417153ff0a`.
Reports and PNGs stay outside tracked source. The unchanged ordinary APK and cold
fixture also passed on the existing native ARM64 builder on 2026-10-07. Both
ordinary journeys retained Unicode/existing-sibling assertions; the cold journey
required initial and safely evicted backend absence, two actual APK restorations,
exact output bytes and denied former metadata/read/write authority with the source
resident. The app's private URI bytes remained unchanged through cold and revoked
relaunches. This proves real FUSE/ART on each remote host with software graphics,
not a signed OS image, multiple physical machines, native Btrfs projection or
physical GPU.

The native FUSE binary and production inspection helper were built remotely from
main `f152ce3010`, with SHA-256 identities
`7d14870afe49281c44b75f3540d3b767673276d895f1d58671c56c2c585d7cdf` and
`ddba6614b80eee9cab51667aba97a74ccf26193750f232c1054af0dbde1dac18`.
The shell/launcher and runtime are the pinned native inputs below. A separate
`run_foreground_journey.py --cold-hydration` run starts with an absent recipient
manifest/chunk, requires projected state before the actual worker read and local
state before any harness read, then checks accepted revocation interrupting open
descriptors. The notice disappears, writes stop, former metadata/read/write access
fails on relaunch, private visits remain ordered and stopped-store inspection
verifies the exact source bytes. All owned FUSE mounts and processes stop. This
does not establish withdrawal of an independent filesystem share or persisted
Android URI grants.

## Started-service lifecycle

The owned SDK-built `ServiceActivity` APK reproduced a concrete runtime defect:
two `startService` calls both received request ID zero, an older
`stopSelfResult` returned true, and neither completion invoked `onDestroy`.
Android requires a distinct start-request token and rejects a stop for an older
request. See the [Android Service API](https://developer.android.com/reference/android/app/Service#stopSelfResult(int)).

The bounded framework patch now reserves increasing IDs before main-looper
dispatch, verifies the registered service instance when stopping, and calls
`onDestroy` on the main looper. It changes neither launcher permissions nor host
authority. All eight modified upstream sources are hash-pinned before mutation.

On 2026-10-06, the unchanged signed APK passed on the patched actual ART runtime:
IDs 1 and 2, stale stop false, current stop true, exactly one main-looper service
destruction, ordered activity pause/stop/destroy, three preserved visits in one
workspace and a fresh independent visit in another. Every launch presented an
Android frame through the production Tilo compositor with enforced Landlock and
no-new-privileges. Camera permission remained denied and no grants were added.
Run `tools/android/run_service_journey.py --help` for the pinned before/after
journey; source and build instructions are in `tools/android/mediation/README.md`.
Generated reports, original screenshots, APKs and signing material stay outside
tracked source.

This qualifies a self-stopping started service in the existing isolated APK
process. Sticky restarts, bound-client lifetimes, process-loss recovery and service survival after closing an activity
still need separate app journeys. This run did not exercise new file grants or
their revocation, signed OS admission, physical devices or ARM64.

On 2026-10-08, the unchanged signed `QueuedServiceActivity` APK reproduced a
second defect on actual native ARM64 ART: two starts followed immediately by
`stopService` returned false, and both canceled commands subsequently ran.
The [Android service implementation](https://android.googlesource.com/platform/frameworks/base/+/master/services/core/java/com/android/server/am/ActiveServices.java)
tracks accepted starts separately from running instances and clears pending work
when bringing a service down. For this ATL main-looper path, the patch tracks
pending request IDs and fences canceled callbacks before creation and dispatch.
Later starts keep increasing IDs; a repeated stop with no work returns false.

The same APK then passed with the changed framework: immediate stop true,
neither canceled command dispatched, subsequent starts 3 and 4 completed, and
both created instances were destroyed exactly once on the main looper. Three
close/relaunch visits preserved the first workspace's private history; another
workspace began independently. Frames, ordered activity lifecycle, denied
camera permission, Landlock and no-new-privileges were checked. The original
service fixture also passed, as did the unchanged foreground fixture's ongoing
notification ownership, normal stop/restart, open-descriptor interruption after
grant withdrawal, stable output bytes and denied revoked relaunch.

The queued APK SHA-256 is
`e09ccaec3e0bd3e25efa3a792874feccbf7b42f7df0974caf993c66ec1256cb1`;
the changed ARM64 runtime receipt is
`398b3b7ef6a7e2f34c2cdab5decb8519127fb9c25b1476a383402bdedad930dc`.
Use `run_service_journey.py --queued-starts` with the pinned inputs and the
explicit queued activity. Reports, images, signing material and runtime payloads
remain outside Git. These runs used the existing remote builder with software
graphics; physical hardware, signed OS admission and bound-service lifetime are
still untested by this case.

## Foreground worker and live grant withdrawal

The owned SDK-23 `ForegroundActivity` APK reproduced two defects on current
main's actual ART runtime: `startForeground` accepted invalid arguments and
returned without sending its notification, and revocation left the running
sandbox's already-open output descriptor writable. The baseline wrote six more
ticks after withdrawal; relaunch denied the former file authority.

The bounded framework patch now requires a nonzero notification ID, a notification
and the existing filtered host provider. It marks the ongoing notice through
[GIO's category API](https://docs.gtk.org/gio/method.Notification.set_category.html).
The compositor makes that notice persistent only for an authenticated Tilo caller
and tracks its unique bus owner. Another sender cannot replace or close it;
owner disconnection withdraws it. Service destruction removes the notice.
Modern foreground permission checks still deny permission, and media/detach
variants explicitly report unsupported behavior. This grants no independent
background execution authority. Foreground sticky process recovery is qualified
separately below; intent redelivery remains unsupported.

Accepted filesystem policy publication now interrupts supervised immutable
sandboxes retaining revoked grants, rather than leaving old open descriptors
usable. The original process identity and durable room scope this interruption;
new restrictions and grant expiry use the same route. A queued launch rechecks
authority before its gate opens. Private per-launch capability files stay with
the supervisor; concurrent launches of the same package cannot overwrite each
other's grants. Policy acceptance is asynchronous, so this is
not a claim that the control request itself synchronously closes descriptors.

On 2026-10-06 the unchanged signed APK passed with the changed framework and
production compositor: a persistent notice outlasted the ordinary toast lifetime,
normal worker stop withdrew it, accepted grant withdrawal interrupted the old
sandbox and removed its notice, and relaunch denied metadata/read/write while
preserving three private visits. The worker returned `START_STICKY`, but authority
withdrawal caused no automatic restart. An unrelated real bus client could not
close or replace the notice or gain persistence by supplying the category.
Only the fixture's new source-read and output-write grants were used and revoked.
Run `tools/android/run_foreground_journey.py --help`; generated JSON, logs, APKs
and screenshots belong under `target/` or outside source.

This qualifies a scoped worker in the existing APK process, with software graphics
on remote x86_64 and native ARM64. It does not establish foreground media/typed service support,
sticky process recovery, service survival after activity closure, signed OS image
admission or physical-device success.

## Foreground sticky process recovery

Android's [START_STICKY contract](https://developer.android.com/reference/android/app/Service)
recreates a started Service after process loss and delivers a null Intent when
there is no pending new command. Reopening the application's Activity does not
prove that contract. The new owned fixture records both service starts and
Activity visits in private storage. Its unchanged signed APK reproduced missing
recovery on the previous runtime: killing ART stopped the worker permanently,
while private state and denial after grant withdrawal remained intact.

The private entrypoint now remains in the original launcher sandbox, creates a
launch-local anonymous memfd journal, and supervises ART. The framework journals
only running foreground Services returning START_STICKY. After abnormal signal
loss, the entry validates a bounded journal and retries at most three times with
a two-second backoff. The framework validates service names against this APK's
enabled declarations, uses a visible recovery surface, and calls each Service on
the main looper with null Intent. The app's Activity is not recreated. The
recovered service must pass the existing foreground/notification admission.
Normal exit or termination, normal service stop and empty/malformed journals do
not restart. Journal state is never persisted across explicit launches.

The real ART journey on remote x86_64 and native ARM64 passed with the identical signed APK:
new process, null Intent, unchanged Activity visit count, resumed scoped work,
presented recovery surface and persistent foreground notice. During a second
process-loss backoff, accepted grant withdrawal killed the original sandbox;
there was no further worker creation, notification or descriptor write. Explicit
relaunch denied former metadata/read/write authority and retained private visits.
Only newly created disposable fixture files received grants, all withdrawn.
Run `tools/android/run_sticky_journey.py --help`; reports and screenshots stay
private outside Git. Six real fork/exec tests additionally check normal exit,
termination, empty/malformed journal, retry and the crash bound.

This qualifies foreground START_STICKY recovery after signal process loss in the
existing authority scope. It adds no independent background privileges. Intent
redelivery, ordinary background services, recovery after closing the activity or
compositor, modern typed/media services, signed OS admission and real devices
remain unqualified. Native ARM64 software graphics do not establish physical GPU
or input acceptance.

## ARM64 qualification path

The official Alpine ARM64 repositories provide the same pinned
[ATL package](https://pkgs.alpinelinux.org/package/edge/testing/aarch64/android-translation-layer)
and ART version as the x86_64 baseline. `acquire_alpine_runtime.sh` accepts an
explicit `aarch64` target. On an x86_64 builder it stages a separate root with
apk signature verification and package scripts disabled, using the target's
architecture-specific keys from the immutable official image's alpine-keys
package. It neither registers binfmt nor installs host packages. Cross-architecture
staging does not execute ART and is not runtime acceptance.

The current entrypoint was cross-linked statically against signed ARM64 musl and
GCC compiler builtins, and the current JNI bridge was cross-linked against signed
ARM64 GTK/GLib headers and libraries with generic signed OpenJDK JNI headers.
Both actual ELF machine IDs are AArch64. The same architecture-independent DEX
framework and unchanged signed APKs pass ARM64 receipt/signature admission.
The admission reports retain `runtime_accepted=false`; separate native journeys
establish actual ART execution. Cross-build and staging alone do not establish
execution. The retained root has also now rendered the fixture surfaces natively
with its staged schema/cache inputs; arbitrary acquisitions still need execution
qualification before device use.

On 2026-10-07, the existing native ARM64 builder reported `aarch64` and
kernel `6.18.54-talos`. Production shell and launcher built from main
`e40be46f15fea4b2fe323c431d8ccf1389714310` ran all five unchanged signed fixture
journeys with software Vulkan graphics. Every journey required fully enforced
Landlock and no-new-privileges. They confirmed service request IDs and main-looper
destruction, ordered Activity lifecycle and room-private state, persistent
foreground notification ownership, notification-capability denial, interruption
of an open-descriptor worker after accepted withdrawal, denied relaunch access,
null-Intent sticky recovery without recreating the original Activity, revocation
during recovery backoff, denied Android permissions and authenticated chooser
cancellation. All owned compositors and buses stopped.

The native runtime receipt is
`27a17c231e2b490e93e2eb9e64423d52620926556b234e56816619c0696c5291`;
shell and launcher SHA-256 identities are respectively
`9cbbc20e95cab37675a0bdb9b212cc0040a28f62a1d3fdf76a20a9641092282c` and
`b886c90fab8afe7b0a2963a5305e12c37b26b52c5c95485382b1e688dcc4787e`.
Reports, original frames, reproducible commands and runtime inputs remain in a
private qualification bundle outside Git. No host package installation, binfmt
registration, new persistent access grant, device takeover or reboot was needed.

Native execution exposed a helper-cleanup race: `/proc/<pid>/environ` can become
unreadable during exit before the captured pidfd is ready. Cleanup waits at most
one second for proof of that exact exit before treating the process as gone. An
unreadable helper that stays live still fails; six real-process/pidfd regression
tests cover ownership, preservation and exit
races, with injected environment-read errors for privilege-independent coverage.

`build_arm64_entry.sh` and `mediation/build_arm64_bridge.py` provide the remote
cross-build commands for a signed development sysroot (musl-dev, gcc, gtk4.0-dev
and glib-dev). The existing `stage_alpine_runtime.py`, `stage_runtime.py` and
`qualify_apk.py` validate ARM64 ELF IDs and complete separate receipts. Preserve
the full upstream GPL source and package provenance, and retain an accepted
native ARM64 Tilo image from the existing
`Build / OS dev (aarch64)` pipeline for physical acceptance. The execution drivers default to the observed Linux host architecture and accept
`--architecture aarch64`; a mismatching execution host fails before launching.
To reproduce on a native remote ARM64 session, run the same
unchanged signed APK journeys through the production launcher/compositor with
mandatory confinement, frames, file/permission denials, notification ownership,
lifecycle and sticky recovery. A loader or emulator probe alone is insufficient.
When transporting private fixture inputs across builders, extract with
`--no-same-owner` inside the receiving user's private directory; preserve modes
and revalidate all runtime and APK hashes before launching. The harness expects
this worktree's binaries at `target/debug/{shell,tilo-launcher}`. If an existing
native builder uses another artifact subdirectory, select its verified AArch64
binaries through links within this same owned worktree; never share another
worktree's target or set `CARGO_TARGET_DIR`.
Yoga/Pi image installation, kernel/security changes, reboots and physical
GPU/input acceptance are coordinated through the parent task; none are performed
by this Android task.

## Compatibility assessment

| Area | Confirmed | Remaining acceptance |
| --- | --- | --- |
| Graphics/input | Actual Arity DEX/ART execution, tiled Wayland tab, OS chrome, software rendering, pointer field focus and keyboard calculation; native ARM64 fixture surfaces | Real GPU/DRM, resize/rotation, ARM64 calculation/input, touch, IME, Back; ATL focus/widget quirks and unsupported listener methods |
| Files/state | App-private history and separate room state; x86-64 and native ARM64 document metadata/read/save, Unicode names and editor modes, FUSE cold read, safe eviction, recipient restart and APK restoration; read-only/sibling denial; revoked metadata/read/write denied with resident source; native ARM64 private URI/visit preservation and withdrawal interrupting open descriptors after a cold FUSE read | Android content providers, Android sharing intents, persisted URI authority, independent filesystem-share withdrawal, native projection and legacy external storage mapping |
| Notifications | Actual filtered-provider delivery/withdrawal with authenticated room attribution; unavailable without capability; ongoing notice persistence, owner cleanup and unrelated-sender denial | Full Android notification tags, actions, resource mapping and independent background persistence |
| Permissions | Strict launcher policy, no requested storage/phone grants, isolated networking | Capability-backed Android permission consent, camera/mic/service grants remain |
| Lifecycle | Actual Android close/reopen and workspace return, saved state, explicit pause/stop/destroy callbacks; started-service request IDs and stale/current self-stop with main-looper destruction; scoped foreground worker stop and revocation interruption; bounded null-Intent sticky recovery and revocation during backoff; owned compositor teardown | Workspace stop, compositor loss, bound/typed/media services, broader recovery, updates and migration |
| Security | Production launcher and authenticated surface adoption; mandatory fully enforced Landlock and no-new-privileges; negative file/network fixture | Malicious JNI/ART probes, cross-room input/bus/clipboard, broader native attack surface review |
| Collaboration/accessibility | Foreign surface display, semantic shell actions, real picker AT-SPI actions and external Android TextView content description | APK state is not system-owned CRDT state; complete Android semantic resource/action and accessibility coverage |

ATL needs no Binder container or kernel change for this journey. The laptop's
existing Waydroid remains stopped and unused. A future Waydroid path needs an
assembled Tilo kernel/architecture check (Binder/Binderfs, namespace/cgroup and
render-node availability) and a per-room isolation design. Neither the laptop's
kernel config nor Vinix's adaptations proves that path. Device/kernel changes,
provisioning and any persistent security grants require their own authorization.
