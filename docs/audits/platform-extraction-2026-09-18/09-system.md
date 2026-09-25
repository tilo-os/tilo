# A09 — Independent system services platform-extraction inventory

## Scope and method

Scope: `crates/system/`, including subcrates, manifests, native shims, scripts, examples, and tests. `app-state` is included as a persistence service without proposing movement of its CRDT or application semantics. The audit covers target guards, FFI and syscalls, processes, host paths, D-Bus, native linking, permissions, locking, and hardware interfaces. Findings describe source contracts and likely portability gaps rather than observed runtime failures. The platform boundary follows `docs/architecture/platform-services.md`.

## Actionable findings

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A09-01 — Durable/private file replacement is already centralized, but exposes Unix semantics

Evidence: `crates/system/atomic-file/src/lib.rs:97` documents Unix mode bits; `:104` explicitly makes mode a no-op off Unix; `:136` opens and syncs the parent directory; `:158`/`:167` gate permission handling. Platform: Unix vs non-Unix. Classification: **existing reusable low-level backend with portability contract gaps**. All persistent service consumers inherit this contract; replacing the implementation with generic write/rename loses durability and privacy.

Boundary: `DurableFiles::replace(path, bytes, privacy)` and explicit directory-sync/private-create capabilities. Preserve the public facade; move target-specific creation, replace and durability implementation into platform storage. Replace raw `u32` modes at portable call sites with semantic privacy while retaining Unix mode support internally. Acceptance: existing atomic replacement tests plus each supported OS's replacement-over-existing, crash/error-after-publication, private-from-creation and directory durability behavior; unsupported guarantees must be reported rather than silently relaxed.

### A09-02 — Persistence locking, file identity and canonical-path ownership

Evidence: `crates/system/app-state/src/lib.rs` functions `same_file`, `acquire_gate`, `acquire_writer_lock`, and no-replace `fs::hard_link` publication; `crates/system/accounts/src/status.rs:155` uses flock; `crates/system/desktop-policy/src/storage.rs:9` imports Unix metadata and later uses device identity for storage scanning. App-state already has Unix flock and Windows exclusive share-mode alternatives, while non-Unix `same_file` returns false and exclusive canonical gates report unsupported. Classification: **extract OS mechanisms, retain persistence policy**.

Boundary: process-lifetime shared/exclusive file leases, stable file identity, durable publish-if-absent, and volume identity. Move these mechanisms together; preserve app-state's migration ownership, CRDT storage and busy/error semantics. Storage accounting must not replace device identity with path string comparisons. Acceptance: multi-process lock contention/release after crash, same-path replacement identity, no-replace concurrent publication, migration winner consistency, and symlink/cross-volume cleanup refusal on each supported backend. Associated autosave module is portable orchestration; do not move it wholesale.

### A09-03 — User/config/cache/runtime directories and process identity leak into policy stores

Evidence: `settings-store/src/lib.rs:61` deliberately implements absolute-XDG config resolution; `settings-store/src/hardware.rs:141` reads `/proc/{pid}/stat`, and `:276` requires XDG_RUNTIME_DIR; `desktop-policy/src/defaults.rs:362`, `desktop-policy/src/storage.rs:22`, `accounts/src/lib.rs` config-root resolver, `power/src/history.rs:95`, and projector persistence resolve their own roots. Platform: Linux/XDG, with some `dirs` host-native behavior. Classification: **extract paths and live process identity, preserve settings schema**.

Boundary: `UserLocations` (config/data/cache/state/private-runtime) and `ProcessIdentity` (PID plus anti-reuse token). Reuse one resolver and migrate stored paths deliberately; preserve settings-store's rejection of relative XDG values. Do not encode Linux start-time ticks as universal identity. Acceptance: all services resolve one intended root per platform; unset/relative environment cases, no unsafe shared-runtime fallback, process exit/reuse invalidates stale records, and persisted schemas remain compatible. `continuity`, notification-policy and roaming-provenance logic are not OS backends merely because they contain path-shaped sample data.

### A09-04 — Bounded child execution must keep process-tree cancellation semantics

Evidence: `bounded-command/src/lib.rs:292` creates Unix process groups; `:324` kills the group only on Unix, then kills/waits the immediate child; readers join after exit. `os-actions/src/process.rs` is the shared facade, but firmware/privacy, projector, system-extension and TTS also manage long-lived subprocess groups. Classification: **central platform process backend plus duplicated mechanism**.

Boundary: bounded captured process and streaming process with owned process-tree lifetime, stdin-secret transport, cancellation, output limits, and deterministic reap. Keep bounded-command's API/error semantics; move per-OS process containment beneath it, then reuse for streaming callers where requirements fit. A Windows implementation needs equivalent containment, not merely child.kill; surviving descendants may retain pipe handles. Acceptance: grandchildren are terminated, pipe readers and stdin writers finish after cancellation, limits/timeouts remain enforced, secrets do not appear in argv, and unrelated processes are never targeted. Unix tests currently use sh/head/cat, `/dev/zero` and executable permission bits; add portable fixture binaries rather than weakening them.

### A09-05 — Hardware/session action facade mixes portable contracts with Linux commands

Evidence: `os-actions/src/lib.rs:151` exposes `Dispatcher`, but default methods invoke concrete Bluetooth/network helpers. Backends: audio `:15` wpctl; network `:42` nmcli, `:594` `/proc/net/wireless`, `:727` NetworkManager D-Bus; modem `:41` mmcli and D-Bus SIM PIN; display `:29` backlight/IIO sysfs plus brightnessctl; battery `:54` power_supply and `:206` vendor charge-cap sysfs; power `:42` powerprofilesctl; printing `:13` CUPS test files and `:177` scanimage; time `:20` timedatectl and `:156` date; users `:12` AccountsService; session `:36` loginctl, `:42` GDM busctl, `:52` systemctl; firmware `:170` fwupdmgr.

Classification: **deliberate Linux backends currently exposed through a mixed shared crate**. Boundary: semantic capability families (audio endpoints, display brightness/sensors, radio/network profiles, modem, battery/charge limits, printing/scanning, firmware transactions, accounts, clock/time-zone and session lifecycle). Reuse models and Dispatcher, extend coverage to getters/watchers, and eliminate default trait methods that reach Linux directly. Keep parsing and command vocabularies in Linux implementations, use explicit Unsupported/Unavailable distinction. Acceptance: fake dispatcher tests exercise production call routing without host commands; Linux integration verifies equivalent authorization, secret handling and subscriptions; another platform can compile/use the facade without Linux dependencies or accidental CLI dispatch.

### A09-06 — Bluetooth and power already have strong separation seams

Evidence: `tilo-bluez/src/client.rs:67` BlueZ object-manager proxy and `agent.rs:24` callback object path; `power/src/lib.rs:43` feature-gates service integration but `sysfs` remains unconditionally exported; logind/UPower/power-profiles monitors and service are D-Bus implementations. `os-actions/src/bluez.rs` retains a connection because discovery/pairing are connection-bound. Classification: **intentional platform backend; retain models/policy above boundary**.

Boundary: Bluetooth discovery/pairing sessions with async challenge responses and retained lifetime; power events, inhibitors and suspend actuator. Reuse bluez model/state and power policy, battery monitor, inhibitor registry, bridge. Move D-Bus addresses, OwnedFd inhibitor lifecycle and sysfs fallback behind Linux backend selection; retain logind sleep/lock ordering. Acceptance: pairing cancel/timeout/passkey paths still work over a persistent connection, discovery stops when its session ends, inhibitors release on owner death, and suspend never races screen locking. `tilo-bluez/examples/live_check.rs` and `os-actions/examples/bt_surface.rs` are intentional live Linux checks.

### A09-07 — Media privacy and trusted client identity are Linux-specific security mechanisms

Evidence: `os-actions/src/privacy.rs:66` reads rfkill and `/dev`; `:111` invokes pw-dump; `:112` resolves trusted process identity through `/proc`; `:129` starts a PipeWire monitor; `:186` group cancellation; `:212` effective UID from proc status. `settings-store/src/hardware.rs:141` has related anti-PID-reuse identity. Classification: **retain security policy; extract Linux observer and identity mechanisms**.

Boundary: authenticated media-use records, hardware mute state, process identity validation and revoke capability. Keep identity derived from host authority, never client-authored labels. Acceptance: forged PipeWire labels do not spoof applications, PID reuse/foreign UID records cannot authorize revocation, hardware kill state distinguishes unknown, and monitor shutdown reaps descendants. Coordinate with platform process/identity rather than exposing `/proc` to callers.

### A09-08 — Gamepad forwarding has reusable semantic interfaces and explicit Linux backends

Evidence: `os-actions/src/lib.rs:47` selects Linux physical input or unsupported substitute; `linux_physical_gamepad.rs:76` `/dev/input`, `:78` `/sys/class/input`; `linux_virtual_gamepad.rs:121` Linux cfg, `:171` ioctls and `:195` `/dev/uinput`; non-Linux backend `:388`. `peripheral_forwarding.rs` declares VirtualGamepad/Factory and authorization/lease semantics. Classification: **intentional well-identified backend**, not a reason to move the forwarding broker wholesale.

Move evdev/uinput implementation and native deps beneath platform input; reuse the factory/device interfaces and preserve explicit unsupported backend. Acceptance: capability enumeration, event mapping and device destruction remain covered; authorization/revocation tests operate on fake devices; non-Linux paths report unsupported and never fake successful input injection.

### A09-09 — Tool launching and desktop application discovery are freedesktop integrations

Evidence: `os-actions/src/tool_intent.rs:8` BorrowedFd/UnixStream transport and `:14` inherited service FD; `desktop-entry/src/scan.rs:97` Flatpak exports and `:117` XDG data dirs; `:328` executable permission bits; `launch.rs:44` session environment allowlist, `:95` desktop name and `:153` process construction. Classification: **extract integration transport/launch backend; retain typed intent security**.

Boundary: typed tool-service channel and host-application registry/launcher. Keep freedesktop entry/Exec parsing (entry, exec, locale, cache) reusable under its backend; do not pretend desktop IDs or Wayland/X11 environment describe every host. Preserve explicit launch environment and authority stripping, command-as-data validation, and inherited-capability ownership. Acceptance: tool protocol cannot select arbitrary programs, launch env does not leak Tilo authority, scans obey precedence/hidden/TryExec, and platform-specific launch configuration is selected without UI conditionals.

### A09-10 — Credential custody is Secret Service-specific with local-key fallback

Evidence: `accounts/src/vault.rs:12` documents custody, `:95` selects service/local storage, `:226`, `:287`, `:313` invoke secret-tool. `status.rs:9`/`:155` Unix lock; atomic-file writes local secret mode. Classification: **extract native credential backend, retain encrypted vault and grant policy**.

Boundary: `CredentialKeyStore` with explicit custody strength, load/store/delete, unavailable vs locked/failure results and stable item identifiers. Reuse SecretCustody reporting and existing secret-broker scopes. Provide Linux Secret Service, then native keychains as required; avoid treating unsupported desktop service as implicit authorization to weaken custody. Acceptance: existing vault ciphertext remains readable, native-service unavailable/locked/error handling is explicit, local-key creation is owner-private on every supported host, no key enters argv/logs, grants and revocation remain unchanged.

### A09-11 — Native TLS trust is deliberately fixed to the Tilo Fedora image

Evidence: `tilo-native-certs/src/lib.rs:16` hardcodes `/etc/pki/tls/certs/ca-bundle.crt`; `:19` always selects it, `:27` rejects certificate directories. Package is a local `rustls-native-certs` compatible subset, not ordinary upstream code to bulk rewrite. Classification: **intentional trust backend requiring platform selection**.

Boundary: platform-owned trust-root provider with a Tilo immutable-bundle policy. Keep the no-environment-redirection invariant for the image; define explicit native-store policy for other hosts. Preserve result error reporting and deduplication. Acceptance: image trust cannot be redirected by environment, missing bundle yields an error, host backend obtains intended roots without silently substituting unrelated files; no blanket replacement with openssl environment probing. Workspace patch/dependency wiring lies outside this scope and must be coordinated.

### A09-12 — TTS factory's purported subprocess portability still depends on Unix process APIs

Evidence: `tts/src/factory.rs:31` gates speech-dispatcher only; `:42` selects espeak unconditionally. `tts/src/espeak.rs:14` imports Unix CommandExt, `:103` killpg; `speechd.rs:18` UnixStream. Cargo has unconditional nix and Unix-only ssip-client. `speaker.rs` already supplies the Speaker trait and queue semantics. Classification: **extract backends, fix selection contract**, without claiming tested build failure.

Boundary: platform speaker discovery/voice enumeration/cancel through existing Speaker interface. Retain queue, null recorder, errors and priority/interrupt model above it; Linux speech-dispatcher/espeak stays as intentional backend and native speech engines can implement the same contract. Acceptance: facade builds without Unix on supported non-Unix target, stop cancels all speech descendants, interrupt priority remains correct, absence is explicitly observable and null is not misreported as audible speech. Factory smoke tests that accept null do not prove real accessibility output.

### A09-13 — Windows appliance is a Linux host service, not a Windows-native host backend

Evidence: `windows-vm/src/libvirt.rs:15` fixed media paths, `:189` `/dev/stdin`, `:578` guest RNG device; config `:73` maps aarch64 to Arm and every other host arch to x86_64; graphics `:91` sysfs; portal_files `:12` Unix FD/permissions, `:85` UID runtime root, `:420` O_NOFOLLOW; https_guest `:13` Unix metadata, `:1298` UID/group checks; controller `:17` root paths and `:428` flock; rail `:12` Unix permissions, `:161` FreeRDP and `:163` bubblewrap. Classification: **intentional Linux virtualization/security backend with portable orchestration seams**.

Boundary: appliance lifecycle/hypervisor, graphics-assignment capability, capability-bound document handles, trusted guest bridge custody, RemoteApp presentation and egress enforcement. Reuse existing CommandRunner, GuestAgent, CapabilityBroker, RemoteAppLauncher and lifecycle traits; keep appliance/publication state machines, catalog schema, guest-network HTTP policy and launch authorization above boundary. Move libvirt/QEMU/sysfs/BWRAP/Unix trust enforcement as a coherent Linux implementation; don't replace confined documents with arbitrary host paths. Make architecture availability explicit rather than treating any non-Arm target as x86.

Acceptance: default-deny network/device/file grants remain enforced, lease expiry/revocation closes projections, symlink and wrong-owner secrets fail, guest certificates remain pinned, lifecycle/presentation tests run with fakes and Linux end-to-end verifies libvirt/RAIL confinement. Non-Linux host support must supply equivalent mechanisms or return unavailable. `examples/domain-xml.rs` is target-specific render verification, not host runtime detection.

### A09-14 — Projector combines portable protocols with Linux discovery/capture process management

Evidence: `projector/src/main.rs:26` Unix listener/permissions/process groups; `:190` NetworkManager P2P D-Bus; `:358` private runtime directory; `:908` tilo-doubletake helper; `:1000` tilo-fluxcast helper; `:1156` group SIGTERM; `:1417` UnixStream. Dependencies include libc/zbus plus portable FCast and mDNS. Classification: **split Linux media capture/discovery backend from projection protocol/state**.

Boundary: display-projection discovery, consent-bound capture session, encoded stream source and process lifetime. Retain portable device/session/FCast logic from lib/main where possible, including HTTP token/path restrictions; move NetworkManager, capture helper argv and Unix sockets behind explicit Linux implementation. Acceptance: close/cancel tears down capture and descendants, private sockets keep permissions, projection token/traversal protections remain, mock P2P bus tests exercise production discovery, portable transports do not require Linux bus dependencies.

### A09-15 — Image boot/rollback/extension tools should remain explicit Linux deployment backends

Evidence: `tilo-cli/src/main.rs:49` deployment helper, `:149` `/proc/self/status`, `:196` systemctl; `system-extension/src/bin/tilo-system-extension-manager.rs:21` image catalogs/runtime paths, `:27` PCI sysfs, `:575` bootc, `:618` uname, `:546` process-group kill; rollback generation `:93` proc cmdline, keyslots `:51` and lockout `:292` `/dev/urandom`, approval `:75`, confirm `:59` persistent paths; rollback exec `:103` tools via existing Runner. `tilo-crypto/src/main.rs:48` Linux openat2 and `:794` prctl; its library implements cryptographic formats rather than host syscalls.

Classification: **retain target-specific deployment/security tooling**, extract reusable mechanisms only. Group rollback TPM2 NV, PCR sealing, cryptsetup/LUKS2 and boot generation operations as Linux-image capabilities, not generic desktop services. Keep signed-extension manifest/transaction logic and crypto-format code independent. Reuse Runner/RealRunner for executable transport; move secure no-symlink key reads and nondumpable process setup into platform security where shared. Replace `/dev/urandom` only with an equivalent OS CSPRNG capability, never a weaker source. Acceptance: existing recorded-runner TPM command tests and real target-image validation retain monotonic floor, approvals, lockout and secret redaction; secure file opens reject symlinks throughout traversal; unsupported hosts cannot perform weakened rollback operations. Image scripts/service/polkit units are adjacent OS scope.

### A09-16 — Tinker and optional swipe native build have explicit Linux/host-toolchain contracts

Tinker evidence: `src/idmap.rs:64` unshare and exec, `src/session.rs:113` proc/dev/run mounts and overlay args, `src/main.rs:263` bwrap and `:471` podman, `src/diff.rs:242` Unix device whiteouts and `:263` Linux lgetxattr; store `:312` logical root stripping. Classification: **intentional Linux sandbox backend**. Boundary: isolated work-layer lifecycle, subordinate-ID mapping, overlay diff semantics. Keep namespace/bwrap/whiteout implementation together; preserve mount and sharing policy, not a generic shell escape hatch. Acceptance: no host-root bind, no authority escalation, discard/revert handles whiteouts/opaque directories, namespace-owned files can be cleaned, unsupported platforms fail explicitly. Demo script remains Linux tooling.

Swipe evidence: `swipe/build.rs:32` prebuilt `cmake-out-host` libraries; `:131` onward hardcoded stdc++/pthread/dl/m links; `src/lib.rs:44` FFI; `setup-futo.sh:26` make host. Classification: **native dependency/build backend**, not automatically an OS API leak. Keep portable decoder API/stub, isolate target/link configuration and validate archive target ABI; use target-aware runtime libs before enabling FUTO elsewhere. Do not move or rewrite upstream ExecuTorch/FUTO internals. Acceptance: missing optional assets still selects documented stub; configured artifacts match Cargo target; enabled decoder executes its real tests rather than treating cfg-disabled tests as proof. README/C++ shim/setup and integration test belong to this extraction inventory.

## Intentional/test-only/false-positive distinctions

- Linux backend names and `/org/...` D-Bus object paths are intentional protocols; moving every path literal to a constants file would not create a platform boundary.
- Tilo image deployment and boot-security programs remain image-specific, with build/dependency selection preventing them leaking into host-neutral libraries.
- `tilo-startup-metrics/src/lib.rs` has `/tmp` in documentation but runtime `init` requires an explicit output environment path; timing/JSON code is portable and does not need platform extraction on this evidence.
- Pure crypto formats (PE/EFI/Authenticode), guest Windows schemas, CRDT persistence policy, forwarding authorization, battery policy, settings continuity and desktop-entry parsing are not OS syscalls.
- Unix fixture permissions, symlinks, shell probes and D-Bus examples need platform-scoped testing or portable subprocess fixtures. Do not ignore failures or reinterpret absent hardware/backend as a passing observation.
- All manifest target dependencies must move with their backend; unconditional libc/nix/zbus dependencies in os-actions, TTS, accounts, projector, windows-vm, tinker, crypto and extension crates require target/features review. A dependency alone does not prove a compilation failure.

## Per-file inventory

The table below includes all scoped files so implementers can distinguish candidates from associated portable policy. Anchors are representative lexical hits and may be comments or tests; findings above establish production significance. All paths are repository-relative. Associated files without a native lexical hit use `:1` for navigation.

| File and representative anchors | Finding / disposition |
|---|---|
| `crates/system/accounts/Cargo.toml:20` | A09-02/03/10; dependency/build ownership accompanies backend |
| `crates/system/accounts/src/lib.rs:17`, `crates/system/accounts/src/lib.rs:330`, `crates/system/accounts/src/lib.rs:335` | A09-02/03/10; native candidate; inspect production/test context |
| `crates/system/accounts/src/secrets.rs:819` | A09-02/03/10; native candidate; inspect production/test context |
| `crates/system/accounts/src/status.rs:9`, `crates/system/accounts/src/status.rs:10`, `crates/system/accounts/src/status.rs:155`, `crates/system/accounts/src/status.rs:165` | A09-02/03/10; native candidate; inspect production/test context |
| `crates/system/accounts/src/vault.rs:6`, `crates/system/accounts/src/vault.rs:13`, `crates/system/accounts/src/vault.rs:85`, `crates/system/accounts/src/vault.rs:227` | A09-02/03/10; native candidate; inspect production/test context |
| `crates/system/app-state/Cargo.toml:25` | A09-02/03; dependency/build ownership accompanies backend |
| `crates/system/app-state/src/autosave.rs:811` | A09-02/03; native candidate; inspect production/test context |
| `crates/system/app-state/src/lib.rs:2`, `crates/system/app-state/src/lib.rs:28`, `crates/system/app-state/src/lib.rs:29`, `crates/system/app-state/src/lib.rs:30` | A09-02/03; native candidate; inspect production/test context |
| `crates/system/atomic-file/Cargo.toml:1` | A09-01; dependency/build ownership accompanies backend |
| `crates/system/atomic-file/src/lib.rs:158`, `crates/system/atomic-file/src/lib.rs:160`, `crates/system/atomic-file/src/lib.rs:167`, `crates/system/atomic-file/src/lib.rs:169` | A09-01; native candidate; inspect production/test context |
| `crates/system/bounded-command/Cargo.toml:13` | A09-04; dependency/build ownership accompanies backend |
| `crates/system/bounded-command/src/lib.rs:10`, `crates/system/bounded-command/src/lib.rs:11`, `crates/system/bounded-command/src/lib.rs:180`, `crates/system/bounded-command/src/lib.rs:199` | A09-04; native candidate; inspect production/test context |
| `crates/system/desktop-entry/Cargo.toml:1` | A09-09; dependency/build ownership accompanies backend |
| `crates/system/desktop-entry/src/bin/tilo-desktop-scan.rs:1` | A09-09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-entry/src/cache.rs:1` | A09-09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-entry/src/entry.rs:557` | A09-09; native candidate; inspect production/test context |
| `crates/system/desktop-entry/src/exec.rs:1` | A09-09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-entry/src/launch.rs:5`, `crates/system/desktop-entry/src/launch.rs:9`, `crates/system/desktop-entry/src/launch.rs:59`, `crates/system/desktop-entry/src/launch.rs:60` | A09-09; native candidate; inspect production/test context |
| `crates/system/desktop-entry/src/lib.rs:1` | A09-09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-entry/src/locale.rs:1` | A09-09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-entry/src/scan.rs:3`, `crates/system/desktop-entry/src/scan.rs:4`, `crates/system/desktop-entry/src/scan.rs:6`, `crates/system/desktop-entry/src/scan.rs:24` | A09-09; native candidate; inspect production/test context |
| `crates/system/desktop-policy/Cargo.toml:1` | A09-02/03; dependency/build ownership accompanies backend |
| `crates/system/desktop-policy/src/defaults.rs:362`, `crates/system/desktop-policy/src/defaults.rs:365` | A09-02/03; native candidate; inspect production/test context |
| `crates/system/desktop-policy/src/lib.rs:1` | A09-02/03; associated API/policy/manifest; not automatically an extraction |
| `crates/system/desktop-policy/src/storage.rs:9`, `crates/system/desktop-policy/src/storage.rs:22`, `crates/system/desktop-policy/src/storage.rs:25`, `crates/system/desktop-policy/src/storage.rs:26` | A09-02/03; native candidate; inspect production/test context |
| `crates/system/os-actions/Cargo.toml:35` | A09-04/05/06/07/08/09; dependency/build ownership accompanies backend |
| `crates/system/os-actions/examples/bt_surface.rs:1` | A09-04/05/06/07/08/09; test/example; preserve target-specific coverage |
| `crates/system/os-actions/src/audio.rs:15`, `crates/system/os-actions/src/audio.rs:37`, `crates/system/os-actions/src/audio.rs:48`, `crates/system/os-actions/src/audio.rs:58` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/battery.rs:54`, `crates/system/os-actions/src/battery.rs:111`, `crates/system/os-actions/src/battery.rs:311` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/bluetooth.rs:1` | A09-04/05/06/07/08/09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/os-actions/src/bluez.rs:62` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/display.rs:14`, `crates/system/os-actions/src/display.rs:29`, `crates/system/os-actions/src/display.rs:30`, `crates/system/os-actions/src/display.rs:55` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/firmware.rs:8`, `crates/system/os-actions/src/firmware.rs:62`, `crates/system/os-actions/src/firmware.rs:72`, `crates/system/os-actions/src/firmware.rs:83` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/lib.rs:47`, `crates/system/os-actions/src/lib.rs:49` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/linux_physical_gamepad.rs:11`, `crates/system/os-actions/src/linux_physical_gamepad.rs:12`, `crates/system/os-actions/src/linux_physical_gamepad.rs:76`, `crates/system/os-actions/src/linux_physical_gamepad.rs:77` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/linux_virtual_gamepad.rs:7`, `crates/system/os-actions/src/linux_virtual_gamepad.rs:121`, `crates/system/os-actions/src/linux_virtual_gamepad.rs:126`, `crates/system/os-actions/src/linux_virtual_gamepad.rs:127` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/modem.rs:10`, `crates/system/os-actions/src/modem.rs:12`, `crates/system/os-actions/src/modem.rs:41`, `crates/system/os-actions/src/modem.rs:48` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/network.rs:42`, `crates/system/os-actions/src/network.rs:43`, `crates/system/os-actions/src/network.rs:82`, `crates/system/os-actions/src/network.rs:86` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/peripheral_forwarding.rs:1` | A09-04/05/06/07/08/09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/os-actions/src/power.rs:42`, `crates/system/os-actions/src/power.rs:52`, `crates/system/os-actions/src/power.rs:67` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/printing.rs:177`, `crates/system/os-actions/src/printing.rs:225`, `crates/system/os-actions/src/printing.rs:226`, `crates/system/os-actions/src/printing.rs:245` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/privacy.rs:5`, `crates/system/os-actions/src/privacy.rs:66`, `crates/system/os-actions/src/privacy.rs:111`, `crates/system/os-actions/src/privacy.rs:129` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/process.rs:46`, `crates/system/os-actions/src/process.rs:155`, `crates/system/os-actions/src/process.rs:156`, `crates/system/os-actions/src/process.rs:180` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/session.rs:7`, `crates/system/os-actions/src/session.rs:9`, `crates/system/os-actions/src/session.rs:32`, `crates/system/os-actions/src/session.rs:36` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/time.rs:33`, `crates/system/os-actions/src/time.rs:46`, `crates/system/os-actions/src/time.rs:70`, `crates/system/os-actions/src/time.rs:130` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/tool_intent.rs:8`, `crates/system/os-actions/src/tool_intent.rs:9` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/os-actions/src/unsupported_physical_gamepad.rs:1` | A09-04/05/06/07/08/09; associated API/policy/manifest; not automatically an extraction |
| `crates/system/os-actions/src/users.rs:4`, `crates/system/os-actions/src/users.rs:9`, `crates/system/os-actions/src/users.rs:58`, `crates/system/os-actions/src/users.rs:59` | A09-04/05/06/07/08/09; native candidate; inspect production/test context |
| `crates/system/power/Cargo.toml:23`, `crates/system/power/Cargo.toml:29`, `crates/system/power/Cargo.toml:41`, `crates/system/power/Cargo.toml:43` | A09-03/06; dependency/build ownership accompanies backend |
| `crates/system/power/src/battery.rs:1` | A09-03/06; associated API/policy/manifest; not automatically an extraction |
| `crates/system/power/src/bridge.rs:109`, `crates/system/power/src/bridge.rs:113`, `crates/system/power/src/bridge.rs:119`, `crates/system/power/src/bridge.rs:164` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/config.rs:53` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/history.rs:69`, `crates/system/power/src/history.rs:95`, `crates/system/power/src/history.rs:170` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/inhibit.rs:1` | A09-03/06; associated API/policy/manifest; not automatically an extraction |
| `crates/system/power/src/lib.rs:17`, `crates/system/power/src/lib.rs:31` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/logind.rs:1`, `crates/system/power/src/logind.rs:18`, `crates/system/power/src/logind.rs:19`, `crates/system/power/src/logind.rs:29` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/policy.rs:1` | A09-03/06; associated API/policy/manifest; not automatically an extraction |
| `crates/system/power/src/profiles.rs:9`, `crates/system/power/src/profiles.rs:16`, `crates/system/power/src/profiles.rs:17`, `crates/system/power/src/profiles.rs:24` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/service.rs:14`, `crates/system/power/src/service.rs:160`, `crates/system/power/src/service.rs:161`, `crates/system/power/src/service.rs:258` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/sysfs.rs:1`, `crates/system/power/src/sysfs.rs:26`, `crates/system/power/src/sysfs.rs:133` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/power/src/upower.rs:1`, `crates/system/power/src/upower.rs:8`, `crates/system/power/src/upower.rs:36`, `crates/system/power/src/upower.rs:37` | A09-03/06; native candidate; inspect production/test context |
| `crates/system/projector/Cargo.toml:16` | A09-14; dependency/build ownership accompanies backend |
| `crates/system/projector/src/lib.rs:1` | A09-14; associated API/policy/manifest; not automatically an extraction |
| `crates/system/projector/src/main.rs:26`, `crates/system/projector/src/main.rs:27`, `crates/system/projector/src/main.rs:28`, `crates/system/projector/src/main.rs:185` | A09-14; native candidate; inspect production/test context |
| `crates/system/settings-store/Cargo.toml:15`, `crates/system/settings-store/Cargo.toml:17` | A09-03/07; dependency/build ownership accompanies backend |
| `crates/system/settings-store/src/continuity.rs:1` | A09-03/07; associated API/policy/manifest; not automatically an extraction |
| `crates/system/settings-store/src/hardware.rs:112`, `crates/system/settings-store/src/hardware.rs:129`, `crates/system/settings-store/src/hardware.rs:133`, `crates/system/settings-store/src/hardware.rs:141` | A09-03/07; native candidate; inspect production/test context |
| `crates/system/settings-store/src/lib.rs:8`, `crates/system/settings-store/src/lib.rs:9`, `crates/system/settings-store/src/lib.rs:10`, `crates/system/settings-store/src/lib.rs:11` | A09-03/07; native candidate; inspect production/test context |
| `crates/system/settings-store/src/notification_policy.rs:1` | A09-03/07; associated API/policy/manifest; not automatically an extraction |
| `crates/system/settings-store/src/roaming_provenance.rs:1` | A09-03/07; associated API/policy/manifest; not automatically an extraction |
| `crates/system/swipe/.gitignore:1` | A09-16; associated API/policy/manifest; not automatically an extraction |
| `crates/system/swipe/Cargo.toml:1` | A09-16; dependency/build ownership accompanies backend |
| `crates/system/swipe/README.md:17` | A09-16; native candidate; inspect production/test context |
| `crates/system/swipe/build.rs:4`, `crates/system/swipe/build.rs:57`, `crates/system/swipe/build.rs:122`, `crates/system/swipe/build.rs:124` | A09-16; native candidate; inspect production/test context |
| `crates/system/swipe/csrc/swipe_ffi.cpp:55`, `crates/system/swipe/csrc/swipe_ffi.cpp:150` | A09-16; native candidate; inspect production/test context |
| `crates/system/swipe/setup-futo.sh:26`, `crates/system/swipe/setup-futo.sh:27` | A09-16; native candidate; inspect production/test context |
| `crates/system/swipe/src/lib.rs:44` | A09-16; native candidate; inspect production/test context |
| `crates/system/swipe/tests/futo_swipe.rs:1` | A09-16; test/example; preserve target-specific coverage |
| `crates/system/system-extension/Cargo.toml:1` | A09-15; dependency/build ownership accompanies backend |
| `crates/system/system-extension/src/bin/tilo-system-extension-manager.rs:12`, `crates/system/system-extension/src/bin/tilo-system-extension-manager.rs:27`, `crates/system/system-extension/src/bin/tilo-system-extension-manager.rs:28`, `crates/system/system-extension/src/bin/tilo-system-extension-manager.rs:29` | A09-15; native candidate; inspect production/test context |
| `crates/system/system-extension/src/lib.rs:805`, `crates/system/system-extension/src/lib.rs:847`, `crates/system/system-extension/src/lib.rs:850` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-bluez/Cargo.toml:22`, `crates/system/tilo-bluez/Cargo.toml:26`, `crates/system/tilo-bluez/Cargo.toml:28` | A09-06; dependency/build ownership accompanies backend |
| `crates/system/tilo-bluez/examples/live_check.rs:20`, `crates/system/tilo-bluez/examples/live_check.rs:78`, `crates/system/tilo-bluez/examples/live_check.rs:81`, `crates/system/tilo-bluez/examples/live_check.rs:84` | A09-06; test/example; preserve target-specific coverage |
| `crates/system/tilo-bluez/src/agent.rs:18`, `crates/system/tilo-bluez/src/agent.rs:19`, `crates/system/tilo-bluez/src/agent.rs:77`, `crates/system/tilo-bluez/src/agent.rs:80` | A09-06; native candidate; inspect production/test context |
| `crates/system/tilo-bluez/src/client.rs:4`, `crates/system/tilo-bluez/src/client.rs:13`, `crates/system/tilo-bluez/src/client.rs:14`, `crates/system/tilo-bluez/src/client.rs:46` | A09-06; native candidate; inspect production/test context |
| `crates/system/tilo-bluez/src/lib.rs:1` | A09-06; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-bluez/src/model.rs:9` | A09-06; native candidate; inspect production/test context |
| `crates/system/tilo-cli/Cargo.toml:15` | A09-15; dependency/build ownership accompanies backend |
| `crates/system/tilo-cli/src/main.rs:17`, `crates/system/tilo-cli/src/main.rs:19`, `crates/system/tilo-cli/src/main.rs:147`, `crates/system/tilo-cli/src/main.rs:149` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-crypto/Cargo.toml:1` | A09-15; dependency/build ownership accompanies backend |
| `crates/system/tilo-crypto/src/lib.rs:1` | A09-15; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-crypto/src/main.rs:6`, `crates/system/tilo-crypto/src/main.rs:7`, `crates/system/tilo-crypto/src/main.rs:37`, `crates/system/tilo-crypto/src/main.rs:47` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-native-certs/Cargo.toml:1` | A09-11; dependency/build ownership accompanies backend |
| `crates/system/tilo-native-certs/src/lib.rs:16` | A09-11; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/Cargo.toml:1` | A09-15; dependency/build ownership accompanies backend |
| `crates/system/tilo-rollback/src/approval.rs:75` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/bin/tilo-rollback.rs:31`, `crates/system/tilo-rollback/src/bin/tilo-rollback.rs:39`, `crates/system/tilo-rollback/src/bin/tilo-rollback.rs:90`, `crates/system/tilo-rollback/src/bin/tilo-rollback.rs:126` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/certify.rs:1` | A09-15; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-rollback/src/confirm.rs:59`, `crates/system/tilo-rollback/src/confirm.rs:316` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/counter.rs:24`, `crates/system/tilo-rollback/src/counter.rs:190`, `crates/system/tilo-rollback/src/counter.rs:377`, `crates/system/tilo-rollback/src/counter.rs:479` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/exec.rs:6`, `crates/system/tilo-rollback/src/exec.rs:77`, `crates/system/tilo-rollback/src/exec.rs:103`, `crates/system/tilo-rollback/src/exec.rs:233` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/generation.rs:47`, `crates/system/tilo-rollback/src/generation.rs:86`, `crates/system/tilo-rollback/src/generation.rs:90`, `crates/system/tilo-rollback/src/generation.rs:93` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/keyslots.rs:17`, `crates/system/tilo-rollback/src/keyslots.rs:48`, `crates/system/tilo-rollback/src/keyslots.rs:51`, `crates/system/tilo-rollback/src/keyslots.rs:53` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/lib.rs:102` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/lockout.rs:196`, `crates/system/tilo-rollback/src/lockout.rs:232`, `crates/system/tilo-rollback/src/lockout.rs:268`, `crates/system/tilo-rollback/src/lockout.rs:289` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/policy.rs:211`, `crates/system/tilo-rollback/src/policy.rs:216`, `crates/system/tilo-rollback/src/policy.rs:301`, `crates/system/tilo-rollback/src/policy.rs:397` | A09-15; native candidate; inspect production/test context |
| `crates/system/tilo-rollback/src/retention.rs:1` | A09-15; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-rollback/src/token.rs:1` | A09-15; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-rollback/src/tpm_policy.rs:1` | A09-15; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tilo-rollback/tests/mechanism.rs:35`, `crates/system/tilo-rollback/tests/mechanism.rs:37`, `crates/system/tilo-rollback/tests/mechanism.rs:116`, `crates/system/tilo-rollback/tests/mechanism.rs:130` | A09-15; test/example; preserve target-specific coverage |
| `crates/system/tilo-startup-metrics/Cargo.toml:1` | Portable; documentation-only /tmp hit; dependency/build ownership accompanies backend |
| `crates/system/tilo-startup-metrics/src/lib.rs:1` | Portable; documentation-only /tmp hit; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tinker/Cargo.toml:1` | A09-16; dependency/build ownership accompanies backend |
| `crates/system/tinker/demo.sh:9`, `crates/system/tinker/demo.sh:21`, `crates/system/tinker/demo.sh:35`, `crates/system/tinker/demo.sh:36` | A09-16; native candidate; inspect production/test context |
| `crates/system/tinker/src/diff.rs:134`, `crates/system/tinker/src/diff.rs:135`, `crates/system/tinker/src/diff.rs:136`, `crates/system/tinker/src/diff.rs:137` | A09-16; native candidate; inspect production/test context |
| `crates/system/tinker/src/idmap.rs:8`, `crates/system/tinker/src/idmap.rs:15`, `crates/system/tinker/src/idmap.rs:28`, `crates/system/tinker/src/idmap.rs:52` | A09-16; native candidate; inspect production/test context |
| `crates/system/tinker/src/lib.rs:1` | A09-16; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tinker/src/main.rs:172`, `crates/system/tinker/src/main.rs:263`, `crates/system/tinker/src/main.rs:264`, `crates/system/tinker/src/main.rs:266` | A09-16; native candidate; inspect production/test context |
| `crates/system/tinker/src/session.rs:51`, `crates/system/tinker/src/session.rs:78`, `crates/system/tinker/src/session.rs:79`, `crates/system/tinker/src/session.rs:85` | A09-16; native candidate; inspect production/test context |
| `crates/system/tinker/src/store.rs:78`, `crates/system/tinker/src/store.rs:360`, `crates/system/tinker/src/store.rs:483`, `crates/system/tinker/src/store.rs:492` | A09-16; native candidate; inspect production/test context |
| `crates/system/tts/Cargo.toml:29` | A09-12; dependency/build ownership accompanies backend |
| `crates/system/tts/src/error.rs:1` | A09-12; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tts/src/espeak.rs:14`, `crates/system/tts/src/espeak.rs:103`, `crates/system/tts/src/espeak.rs:104`, `crates/system/tts/src/espeak.rs:105` | A09-12; native candidate; inspect production/test context |
| `crates/system/tts/src/factory.rs:31` | A09-12; native candidate; inspect production/test context |
| `crates/system/tts/src/lib.rs:87`, `crates/system/tts/src/lib.rs:96` | A09-12; native candidate; inspect production/test context |
| `crates/system/tts/src/null.rs:1` | A09-12; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tts/src/queue.rs:1` | A09-12; associated API/policy/manifest; not automatically an extraction |
| `crates/system/tts/src/speaker.rs:37`, `crates/system/tts/src/speaker.rs:197` | A09-12; native candidate; inspect production/test context |
| `crates/system/tts/src/speechd.rs:18` | A09-12; native candidate; inspect production/test context |
| `crates/windows/windows-vm/Cargo.toml:1` | A09-13; dependency/build ownership accompanies backend |
| `crates/windows/windows-vm/examples/domain-xml.rs:17`, `crates/windows/windows-vm/examples/domain-xml.rs:18` | A09-13; test/example; preserve target-specific coverage |
| `crates/windows/windows-vm/src/appliance.rs:753`, `crates/windows/windows-vm/src/appliance.rs:754` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/bin/tilo-windows-rail.rs:12`, `crates/windows/windows-vm/src/bin/tilo-windows-rail.rs:22`, `crates/windows/windows-vm/src/bin/tilo-windows-rail.rs:56`, `crates/windows/windows-vm/src/bin/tilo-windows-rail.rs:161` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs:5`, `crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs:6`, `crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs:17`, `crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs:18` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/command.rs:155`, `crates/windows/windows-vm/src/command.rs:197` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/config.rs:73`, `crates/windows/windows-vm/src/config.rs:215`, `crates/windows/windows-vm/src/config.rs:216` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/graphics.rs:382` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/guest.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/guest_network.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/https_guest.rs:13`, `crates/windows/windows-vm/src/https_guest.rs:250`, `crates/windows/windows-vm/src/https_guest.rs:251`, `crates/windows/windows-vm/src/https_guest.rs:1298` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/lib.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/libvirt.rs:15`, `crates/windows/windows-vm/src/libvirt.rs:16`, `crates/windows/windows-vm/src/libvirt.rs:189`, `crates/windows/windows-vm/src/libvirt.rs:223` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/lifecycle.rs:216`, `crates/windows/windows-vm/src/lifecycle.rs:217`, `crates/windows/windows-vm/src/lifecycle.rs:267`, `crates/windows/windows-vm/src/lifecycle.rs:268` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/portal_files.rs:12`, `crates/windows/windows-vm/src/portal_files.rs:13`, `crates/windows/windows-vm/src/portal_files.rs:14`, `crates/windows/windows-vm/src/portal_files.rs:72` | A09-13; native candidate; inspect production/test context |
| `crates/windows/windows-vm/src/presentation.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/publication.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/remote_app.rs:1` | A09-13; associated API/policy/manifest; not automatically an extraction |
| `crates/windows/windows-vm/src/state.rs:2`, `crates/windows/windows-vm/src/state.rs:146` | A09-13; native candidate; inspect production/test context |

## Cross-scope coordination and validation

Coordinate app callers, shell hardware/privacy/input/session callers, packaging launcher, runtime CLI location resolver, collab capture helpers, OS units/udev/polkit/TPM/image layout, and workspace rustls patch/dependencies with their assigned owners. None were audited in depth here. No observed test/build evidence is claimed. During implementation run focused production-path contract tests and target backend validation, then the repository-required `cargo check -p shell --tests --locked`; retain lockfile and sandbox/authorization invariants. This report has 16 grouped findings, including intentional backends and build/deployment work, plus a complete 147-file navigation inventory.
