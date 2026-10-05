# Android applications on Tilo: qualification and gaps

Status at 2026-10-05, repository base `4b1210dc7c`. This is an opt-in
qualification prototype, not shipped Android compatibility. [Android](android.md)
describes the opposite direction: building a Tilo application for Android.

## Reference and runtime choice

The supplied bookmark name was **Vinix**, without a URL. The likely project is
[vlang/vinix](https://github.com/vlang/vinix), but the bookmark identity remains
unconfirmed. Its [Android documentation](https://github.com/vlang/vinix/blob/master/docs/android.md)
describes native ARM64 ATL/ART execution and reports a calculator acceptance
journey; those results belong to Vinix, not Tilo. Its page-size adaptations are
not a reason to change Tilo's kernel.

[Android Translation Layer](https://gitlab.com/android_translation_layer/android_translation_layer)
implements part of Android's framework over Linux libraries and GTK. Its
[architecture](https://gitlab.com/android_translation_layer/android_translation_layer/-/blob/master/doc/Architecture.md)
keeps APK DEX and matching native libraries. This fits a process per app better
than a shared Android system, but missing framework methods are real compatibility
gaps. ATL is not an Android security boundary: Tilo's launcher must confine it.

[Waydroid](https://docs.waydro.id/usage/install-on-desktops) runs an Android
container and supports individual application windows. It offers broader framework
coverage, but a shared container, shared service state and privileged setup do not
supply Tilo's per-workspace isolation. A container per workspace or a VM boundary
needs separate design and approval before any provisioning. Do not launch the
laptop's existing shared Waydroid instance for this task.

## Current inventory and dependencies

No APK runtime, APK admission pipeline or Android-to-Tilo service bridge was found
in the committed runtime, launcher, compositor or OS image. The existing SDK
Android host is not one. The compositor already hosts foreign Wayland surfaces
and routes their input and streaming; the launcher already constructs per-app,
per-workspace data directories and enforces bubblewrap, Landlock and seccomp.
Use those paths rather than a new launch or input mechanism.

The connected x86-64 Fedora laptop has Waydroid and Android SDK build-tools
36.1.0. `waydroid status` reported `STOPPED`; `/dev/binderfs` was absent; ATL was
not installed. A resumed read of the running laptop kernel config
(`/usr/lib/modules/7.2.7-ogc1.1.fc44.x86_64/config`) confirmed Binder IPC and
Binderfs enabled, with `binder,hwbinder,vndbinder` device names. This is host
Fedora evidence, not evidence about Tilo's assembled kernel.
No module was loaded, service started, emulator started, package installed,
permission granted or device rebooted.

The Tilo kernel fragment does not explicitly select Binder. Waydroid qualification
must inspect the **assembled architecture-specific kernel config**, Binder IPC
and Binderfs availability, namespaces/cgroups, memory sharing, LXC, networking
and the actual render node. Do not infer assembled support from a fragment or
turn off SELinux to get a container running. ATL does not need a Binder container;
it needs a coherent ART/framework/native-helper runtime, host-compatible libc,
matching JNI ABI, graphics libraries and page-size support.

`os/scripts/verify-no-ui-toolkits.sh` deliberately excludes GTK from the base
image. An ATL experiment therefore needs a private runtime containing its GTK
closure and license notices, not GTK installed into the OS. Current upstream
installed-prefix assumptions also need relocation work. No suitable relocated
runtime is available in this session; downloading Alpine packages does not prove
they can execute under the Fedora launcher.

## Qualification path implemented

`tools/android/qualify_apk.py` inspects a checksum-pinned standalone APK without
extracting it. It bounds archive size and entry count, rejects traversal,
duplicate members, links and encryption, requires DEX and a manifest, and checks
selected native libraries' ELF architecture. ARM-only APKs cannot pass x86-64
qualification. Split APK delivery is outside this initial scope. Archive admission
does not parse the binary Android manifest or establish API compatibility.

Preparation additionally requires `apksigner` to verify exactly one pinned
certificate, a pinned runtime receipt, an explicit activity, a workspace and an
existing disposable Wayland socket. It produces a payload, `tilo.toml` and
`qualification.json`, with the exact **argv array** for the ordinary
`tilo-launcher run`. It does not run that command, install a desktop entry,
change the package store or grant access. The payload uses `tilo-default`,
network off, no services or portals, and ART interpreter flags. A permission
failure is a qualification failure, never a reason to choose `run-host`, weaken
seccomp or enable JIT. The content-derived qualification package id gives each
APK revision separate data; update continuity is not implemented.

The caller supplies a native, relocatable runtime root with executable
`bin/android-translation-layer`, every required library/framework/resource file,
and `licenses/` notices. No symlinks, special files or setuid/setgid files are
admitted. The entrypoint must find its runtime relative to itself without host
paths, launcher environment overrides or a new runtime mount. Hashes identify
inputs; a receipt is **not** a publisher signature or a claim that those inputs
work. Review runtime provenance and licenses before qualification. No upstream
runtime or APK is vendored by this change.

A runtime receipt has this shape (the map must cover every regular file):

```json
{
  "schema": 1,
  "architecture": "x86_64",
  "files": {
    "bin/android-translation-layer": "<sha256>",
    "licenses/NOTICE": "<sha256>",
    "lib/example.so": "<sha256>"
  }
}
```

Archive-only inspection:

```sh
python3 tools/android/qualify_apk.py /path/Arity-1.1.apk \
  --apk-sha256 1928e65ced8cbe78be2ff3cb4c321e9e75d138e8e30ea1368fbb772a86827d1d \
  --architecture x86_64
```

Preparation once a reviewed runtime and disposable compositor exist:

```sh
python3 tools/android/qualify_apk.py /path/Arity-1.1.apk \
  --apk-sha256 1928e65ced8cbe78be2ff3cb4c321e9e75d138e8e30ea1368fbb772a86827d1d \
  --architecture x86_64 --activity calculator/Calculator \
  --apksigner "$ANDROID_HOME/build-tools/36.1.0/apksigner" \
  --signer-sha256 830e955c73d02aa077c270fcb3e089a556976afe8e9ab3f9b39c4616fdbd9a6d \
  --runtime-root /path/reviewed-atl-runtime \
  --runtime-receipt /path/runtime-receipt.json \
  --runtime-receipt-sha256 '<reviewed receipt sha256>' \
  --workspace-id android-qualification \
  --wayland-socket "$XDG_RUNTIME_DIR/<disposable workspace bridge>" \
  --output /path/new-qualification-payload
```

The socket existence check does not authenticate a Tilo workspace. Its provenance
must come from the disposable compositor's workspace bridge. Use the argv in the
report only inside that qualification environment. Never point this experiment
at the user's desktop.

## Acceptance matrix and next steps

| Area | Existing foundation | Required acceptance before claiming support |
| --- | --- | --- |
| Graphics | Foreign Wayland surfaces; selected GPU in launcher | APK renders inside one tiled tab, resize/rotation, GPU and software paths on x86-64 and ARM64; remove ATL client title bar through supported upstream window configuration |
| Input | Workspace seat routing and `tilo-control` | Pointer, touch, keyboard, IME/caret, focus and Back; real app result after input, not a helper's computed result |
| Notifications | Tilo notification/portal service | Android notification identity, actions, cancellation and denied access mapped to workspace-scoped Tilo resources/actions; not a host D-Bus notification bypass |
| Files | Private XDG data and capability-gated filesystem | File picker returns approved hydrated content, save persists, second workspace cannot read it, revoked grant fails; no host home or `/var/workspace` blanket bind |
| Permissions | Launcher capability policy and syscall/FS boundaries | Android requested/runtime permissions mapped to Tilo prompts; camera/mic/network denied until granted; unsupported permission reports failure |
| Lifecycle | Launcher process-tree management | Pause/resume, tab close, workspace stop, compositor loss and relaunch; no background process or socket leaks; update identity/data migration |
| Security | Bubblewrap, Landlock, seccomp, workspace bridge | Native JNI escape probes; cross-workspace input, storage, bus and network denial; retain SELinux and strict W^X; unverified input refused |
| Collaboration/accessibility | Foreign surface streaming; resources/actions | Surface sharing does not make APK state a CRDT. Android accessibility and semantic actions require an explicit bridge and attributed authority checks |

The representative first app is **Arity 1.1**, an offline calculator. Source and
APK were taken from its official Google Code archive using the independently
recorded [Vinix pins](https://github.com/vlang/vinix/blob/master/build-support/android/build.py).
The archive SHA-256 was
`1b39e4f968e7d21602ea166f73a1b36c35213b78e9e3e06b74053cfdc98dfbe2`;
the APK checksum is in the commands above. The source archive includes the
application's licensing context; Vinix records Apache-2.0. The APK contains no
native libraries, and archive admission passed for both supported architectures.
SDK `apksigner` verified its v1 signature and the pinned certificate above.
SDK `aapt2 dump badging` also identified `arity.calculator`, activity
`calculator.Calculator`, minimum SDK 3, and legacy storage/phone-state permission
requests (including implied permissions). These requests grant no host access in
the prepared Tilo payload. This establishes provenance checks and input
eligibility, **not execution**.

The runtime journey still to execute: launch through the generated sandbox plan,
use `tilo-control` input to enter `123+456`, assert the APK's visible result is
`579`, capture only the isolated scene, close/reopen and check app data, open the
same APK in a second workspace and prove separation. Use semantic actions for
shell operations; input operations are necessary here until the APK exports an
action/accessibility bridge. Calculator success does not validate notifications,
file pickers or modern API/service-heavy apps; each needs a purpose-built journey.

Hermetic tests cover production admission/preparation functions using synthetic
APK/ELF fixtures and a mocked signature verifier. A real APK's signature was
also independently checked with SDK `apksigner`. No Android runtime, emulator or
real-device result is claimed. Kernel changes, Waydroid provisioning, private
runtime packaging and service bridges remain separate approval/design work.
