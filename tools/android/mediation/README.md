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
needs a real FUSE run; a metadata API result alone is not proof of it.

Notification APIs check Tilo's existing provider over the already registered
GApplication filtered bus. Explicit fixture preparation with `--notifications`
declares only the existing `notifications = "via-host"` portal; the default declares
none. The entry selects the freedesktop backend, which remains behind the launcher
filter. Actual delivery and withdrawal, with authenticated workspace attribution, are
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
acceptance explicitly false. Admitting an ARM64 runtime receipt needs a matching
native Linux execution host, which an x86_64 builder is not. Native ARM64 ART
runs, generated schema/cache inputs and physical graphics/input remain separate
qualification steps.

## Build and stage

Run on the approved remote builder in a **disposable rootless Alpine container**
with two CPUs, 2 GiB memory and 256 PIDs. Use the official immutable base recorded
in the qualification receipt, signed Alpine repositories and pinned
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

Build one signed fixture per acceptance case, each prepared as its own
`qualify_apk.py` payload:

- `AcceptanceActivity.java` alone: launch through the compositor's `applications`
  action, a presented Android frame, private result files, an authenticated
  chooser refusal, and close ordering with Landlock evidence.
- `ServiceActivity.java` and `QueuedServiceActivity.java` with
  `ServiceManifest.xml`: started-service request IDs, stale stops and cancellation
  before main-looper dispatch (activities
  `org/tilo/android/services/ServiceActivity` and
  `org/tilo/android/services/QueuedServiceActivity`).
- `ForegroundActivity.java` with `ForegroundManifest.xml`, prepared with
  `--notifications`: notification visibility, scoped workers, withdrawal of their
  disposable grants and sticky process-loss recovery (activity
  `org/tilo/android/foreground/ForegroundActivity`). The same APK without
  `--notifications` renders while refusing foreground admission and starts no
  scoped worker.
- `IntegrationActivity.java` prepared with `--notifications`: document grants,
  notification delivery and withdrawal, and external accessibility in one run.
- `DocumentActivity.java` with `DocumentManifest.xml`: document metadata and
  editor modes.

Keep a signed APK unchanged between two runtimes, so that a difference between
the runs is the runtime's and not the fixture's. A fixture grants only files it
creates itself, and withdraws them before it finishes.

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
These native tests cover the supervisor alone; they do not replace an ART run.
The scoped `android-runtime-supervisor` CI group compiles and runs the same
production harness automatically; no SDK, ART, network or privileges are needed.

Naming an activity here does not qualify it. The acceptance runs that recorded
what each fixture observed went through the session as it was then: the real
launcher, compositor, ART and an external accessibility client, on x86-64,
software and headless, with every report and PNG kept outside Git. Nothing in
this repository drives these fixtures now; they are buildable inputs, and the
list above says what each was built to watch.

Remaining gaps are platform work rather than missing evidence: persisted URI
authority, revocation of an already-open descriptor, native Btrfs projection,
notification identity, tags, actions and background persistence, compositor loss,
workspace stop, background services and updates, and real-device GPU/DRM, ARM64,
touch and IME, and the complete Android accessibility and resource bridge.

