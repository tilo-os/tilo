# Android applications on Tilo: execution and remaining gaps

Status at 2026-10-06, based on `4b1210dc7c`. **An unchanged Android APK executes
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
| Tested runtime receipt SHA-256 | `804b2c280485ee16b8051a45b43c4a3389260d90ebed78188b9f0dc0b34908ea` |
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

Use the existing approved remote builder and `rcargo --no-fallback` for Rust
builds. Never share a worktree target directory or build locally for this task.
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

## Compatibility assessment

| Area | Confirmed | Remaining acceptance |
| --- | --- | --- |
| Graphics/input | Actual Arity DEX/ART execution, tiled Wayland tab, OS chrome, software rendering, pointer field focus and keyboard calculation | Real GPU/DRM, resize/rotation, ARM64, touch, IME, Back; ATL focus/widget quirks and unsupported listener methods |
| Files/state | Actual Android app-private history saves and reopens, separate positive state in two rooms; native canary denial | Hydrating portal file picker, approved save/share, revoke, legacy external storage API mapping |
| Notifications | Existing Tilo notification/portal service foundation | Android notification identity/actions/cancel mapped to workspace resources; no host D-Bus bypass |
| Permissions | Strict launcher policy, no requested storage/phone grants, isolated networking | Android requested/runtime permission prompts and denial semantics, camera/mic/service integration |
| Lifecycle | Actual Android close/reopen and workspace return, saved state, owned compositor teardown | Workspace stop, compositor loss, background workers, service apps, updates and migration |
| Security | Production launcher and authenticated surface adoption; mandatory fully enforced Landlock and no-new-privileges; negative file/network fixture | Malicious JNI/ART probes, cross-room input/bus/clipboard, broader native attack surface review |
| Collaboration/accessibility | Existing foreign surface display/streaming and semantic shell actions | APK state is not system-owned CRDT state; Android semantic resource/action and accessibility bridges are required |

ATL needs no Binder container or kernel change for this journey. The laptop's
existing Waydroid remains stopped and unused. A future Waydroid path needs an
assembled Tilo kernel/architecture check (Binder/Binderfs, namespace/cgroup and
render-node availability) and a per-room isolation design. Neither the laptop's
kernel config nor Vinix's adaptations proves that path. Device/kernel changes,
provisioning and any persistent security grants require their own authorization.
