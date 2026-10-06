# Android mediation acceptance slice

This is a private runtime integration and an owned SDK-built test APK. It is not
an installed Tilo app, a package-store publisher, or a general Android framework.

`patch_framework.py` checks exact SHA-256 pins for the five modified Java sources
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
Use SDK build-tools 36.1.0 `aapt2 compile --dir res`, then `aapt2 link --manifest
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

For the successful path, prepare `IntegrationActivity` with `--notifications`,
then use `tools/android/run_integration_journey.py --fixture-grants --help`.
The explicit flag authorizes only newly created fixture source-read/output-write
grants. The runner uses an owned bus, selects the real picker controls through
external AT-SPI, checks notification delivery and withdrawal, exact document
bytes, read-only/sibling denials, close lifecycle and external content description,
revokes both grants, then relaunches and requires former URI read/write denials.
It always revokes outstanding fixture grants and stops its owned processes.
See `docs/verification/android-integration` for actual results and original PNGs.

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
- [ ] Cold P2P hydration, persisted URI authority and already-open FD revocation.
- [ ] Full notification identity/tags, actions and background persistence.
- [ ] Compositor loss, room stop, background services and updates.
- [ ] Real-device GPU/DRM, ARM64, touch/IME and complete Android accessibility/resources.

The checked items are bounded x86-64 software/headless execution evidence.
Remaining items are additional platform implementation and qualification scope.
