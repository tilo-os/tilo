# Android applications on Tilo: execution and remaining gaps

Status at 2026-10-06, reconciled with the merged redesign on published main `fc91d3bb7852`. **An unchanged Android APK executes
through Tilo's production compositor and launcher.** Arity 1.1 rendered
`123+456 = 579`, saved its state on tab close, restored it on relaunch, and
rendered `234+567 = 801` with separate state in a second workspace. This is
x86-64 execution on the existing remote Fedora builder with a headless Tilo
compositor and software graphics. It is not emulator evidence, real-device
qualification, ARM64 qualification, or general Android application support.
[Android](android.md) describes the opposite direction: building Tilo apps for Android.

## Reference and runtime

The bookmark name **Vinix** was supplied without a URL. The likely reference is
[vlang/vinix](https://github.com/vlang/vinix); the bookmark's identity remains
unconfirmed. Its [Android documentation](https://github.com/vlang/vinix/blob/master/docs/android.md)
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

The checked-in [acceptance record](verification/android-arity/journey.json),
[rendered calculation](verification/android-arity/calculation.png),
[second-room calculation](verification/android-arity/second-workspace-calculation.png)
and [confinement log](verification/android-arity/confinement.log) come from that
complete run. All five original PNGs and their OCR text are preserved there.
The full APK/framework execution logs and private profiles remain in the owned
remote test workspace rather than publishing app-data or identity directories.

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

The final [Android API acceptance record](verification/android-mediation/journey.json)
and [Arity record](verification/android-arity/journey.json) use the same final runtime
and production binaries as the successful integration below. Both were rerun after
the security fix. Earlier checkpoint `ccad09593611` reached terminal green affected,
quality, SDK, filesystem and legacy workflows; new continuation checks are separate.

## Successful document, notification and accessibility integration

The [complete integration record](verification/android-integration/journey.json),
[Android results](verification/android-integration/acceptance.txt),
[external accessibility tree](verification/android-integration/accessibility.json)
and three original PNGs preserve actual execution evidence. The runner creates only
new disposable source, destination and sibling-canary documents in its own profile.
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
write. `Scope::Workspace` now matches only room resources, as the capability model's
enforcement boundary documents. Production-decider and launcher tests preserve
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
client tests, focused capability-decider and launcher regressions, mandatory remote
shell/launcher test-target compilation, strict remote lint, complete framework and
strict C builds, SDK signature verification, all three actual APK journeys and
visual inspection of the owned fixture PNGs. All Rust builds/tests run remotely
with local fallback disabled.

## Compatibility assessment

| Area | Confirmed | Remaining acceptance |
| --- | --- | --- |
| Graphics/input | Actual Arity DEX/ART execution, tiled Wayland tab, OS chrome, software rendering, pointer field focus and keyboard calculation | Real GPU/DRM, resize/rotation, ARM64, touch, IME, Back; ATL focus/widget quirks and unsupported listener methods |
| Files/state | App-private history and separate room state; approved local document read/save, read-only and sibling denial; revoked URI read/write denied on relaunch | Cold P2P hydration, sharing, persisted URI authority, immediate running-process revocation and legacy external storage mapping |
| Notifications | Actual filtered-provider delivery/withdrawal with authenticated room attribution; unavailable without capability | Full Android notification tags, actions, resource mapping and background persistence |
| Permissions | Strict launcher policy, no requested storage/phone grants, isolated networking | Capability-backed Android permission consent, camera/mic/service grants remain |
| Lifecycle | Actual Android close/reopen and workspace return, saved state, explicit pause/stop/destroy callbacks, owned compositor teardown | Workspace stop, compositor loss, background workers, service apps, updates and migration |
| Security | Production launcher and authenticated surface adoption; mandatory fully enforced Landlock and no-new-privileges; negative file/network fixture | Malicious JNI/ART probes, cross-room input/bus/clipboard, broader native attack surface review |
| Collaboration/accessibility | Foreign surface display, semantic shell actions, real picker AT-SPI actions and external Android TextView content description | APK state is not system-owned CRDT state; complete Android semantic resource/action and accessibility coverage |

ATL needs no Binder container or kernel change for this journey. The laptop's
existing Waydroid remains stopped and unused. A future Waydroid path needs an
assembled Tilo kernel/architecture check (Binder/Binderfs, namespace/cgroup and
render-node availability) and a per-room isolation design. Neither the laptop's
kernel config nor Vinix's adaptations proves that path. Device/kernel changes,
provisioning and any persistent security grants require their own authorization.
