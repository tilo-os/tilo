# Android mediation acceptance slice

This is a private runtime integration and an owned SDK-built test APK. It is not
an installed Tilo app, a package-store publisher, or a general Android framework.

`patch_framework.py` checks exact SHA-256 pins for the three modified Java sources
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

Notification APIs report unavailable and suppress send/cancel while there is no
qualified capability mapping. The entrypoint forces GIO's portal backend, retaining
the launcher's filtered-bus policy. It never requests the raw desktop bus or adds
a manifest portal grant. Notification delivery/actions are not qualified here.

## Build and stage

Run on the approved remote builder in a **disposable rootless Alpine container**
with two CPUs, 2 GiB memory and 256 PIDs. Use the official immutable base recorded
in `docs/android-compatibility.md`, signed Alpine repositories and pinned
`art_standalone-dev=0_git20251009-r2`, plus `openjdk17-jdk`, `build-base`, `python3`.
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

## Acceptance checklist

- [x] Actual unchanged Arity APK calculation, persistence and separate room state.
- [x] Actual SDK-built APK sees storage/camera/microphone/location denial.
- [x] Runtime permission request delivers matching denied callback results.
- [x] Document intent reaches authenticated chooser; no grant yields cancel/no URI.
- [x] Notifications advertise unavailable; no raw-bus fallback or added host grant.
- [x] Real close invokes pause, stop and destroy in order; owned session cleans up.
- [ ] Actual APK selects and reads/writes an already-approved hydrated document.
- [ ] Revocation and persisted URI authority have qualified Android semantics.
- [ ] Notification identity, actions, cancellation and delivery reach Tilo resources.
- [ ] Compositor loss, room stop, background services and updates are qualified.
- [ ] Real-device GPU/DRM, ARM64, touch/IME and Android accessibility integration.

The checked items are bounded x86-64 software/headless evidence. The remaining
items are work still to implement and execute, not an external runtime blocker.
