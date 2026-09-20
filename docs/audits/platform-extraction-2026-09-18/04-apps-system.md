# A04 — System applications platform extraction audit

## Scope and method

Scope: `apps/{backups,containers,disk-usage,disks,file-explorer,firewall,logs,monitor,osk,print-centre,settings,store,themes,vpn,windows}`, including source, manifests, tests, examples, and scripts. The audit covers platform guards, Unix and descriptor APIs, FFI, command construction, native dependencies, host paths, services, portals, hardware, and xattrs. Application-framework internals and dynamic dependency behavior are outside its scope. Findings distinguish production paths from tests and fixtures.

Destination alignment: `docs/architecture/platform-services.md` proposes focused `ui::platform` facade modules implemented in narrow `crates/system` or `crates/runtime` services. Apply every boundary below through that facade; implementation crates must not depend on app crates. Move neutral required types out of apps along with backend code, and keep native build/dependency ownership with the provider. The design is proposed, not implemented. Proposed capability names below are contracts to implement or consolidate, **not claims that an existing platform API exists**. Reuse observed seams: `os-actions`/`Dispatcher`, `bounded-command`, `settings-store`, `runtime-portal`, `runtime-fs::storage`, `app-host::client`, and existing app-local traits. Keep semantic action/resource registration and CRDT/UI logic in the app; move actual OS transport, discovery, privileged execution, and native representation translation behind the platform framework. Never broaden sandbox access to make extraction work.

## Actionable findings

### A04-01 — Files owns the collaborative filesystem's xattr transport and errno ABI (high)

- Evidence: `apps/file-explorer/src/model.rs:50` (sync), `:99`, `:119`, `:155` (metadata), `:317`, `:322`, `:421`, `:426`, `:431` (share/history/availability/hash/identity), `:573`, `:582`, `:602`, `:622`, `:640`, `:667` (record-index protocols), `:1199` (thumbnail), `:1282` (share socket locator); `apps/file-explorer/src/files_app.rs:1951`, `:2003`, `:2049`, `:2093` (pin/evict/availability/retry writes); `apps/file-explorer/src/history_control.rs:38`; `apps/file-explorer/src/framework_history.rs:7`, `:33`, `:101`, `:130`, `:143` (FD-bound xattrs and libc errno translation). Dependencies: `apps/file-explorer/Cargo.toml:58` and `:59`.
- Platform/condition: live collaborative mounts using the Tilo FUSE `user.tilo.*` ABI; xattr support alone does not imply a Tilo authority. This is a deliberate Linux filesystem backend with transport assumptions leaked across app/model/history code.
- Impact: Files must know the mount's wire protocol, Linux error vocabulary, paging generations, and file-descriptor behavior; an alternate host must emulate those mechanisms to use app features.
- Boundary/migration: introduce a typed collaborative-files capability for projection, paged catalogue, exact lookup, availability changes and checked history operations. Preserve collab-types wire/domain types and FD-bound identity checks; put xattr names, serialization, bounded reads, libc mappings, and mount detection in its Linux backend. Do not replace checked operations with path-based unchecked writes or read content merely to determine metadata.
- Acceptance: existing pin/evict/retry, exact lookup, stale cursor, privacy/ACL, checked-restore and identity-after-rename tests run through the production adapter; unavailable, stale, denied, pending writes, and unsupported remain distinct. Retain real FUSE certification separately from fake platform tests.

### A04-02 — Files Unix service channels and path-byte encoding live in app code (high)

- Evidence: `apps/file-explorer/src/sharing.rs:196`, `:210` opens a Tokio Unix socket from the authority locator; `apps/file-explorer/src/received_shares.rs:7`, `:110`, `:118`, `:125` clones inherited service FD and wraps it as UnixStream. `apps/file-explorer/src/nearby_send.rs:91` serializes Unix path bytes into nearby-send requests; `apps/file-explorer/src/model.rs:704` and `apps/file-explorer/src/search_provider.rs:49` hash Unix path bytes. `apps/file-explorer/src/identity_directory.rs:19` supplies a `/run/tilo/identity-projection/...` default.
- Platform/condition: Unix service descriptors/sockets; unguarded Unix imports in production. Windows-style names and non-Unix service handles cannot use these exact representations unchanged; no build was attempted.
- Classification: extraction candidate, not a reason to remove capability confinement.
- Boundary/migration: typed received-shares/share-control clients accepting inherited verified service capability, plus platform-local opaque path identity/encoding and trusted-identity projection resolver. Retain bounded framing, timeouts and cloning/ownership rules. Decide path wire format explicitly; do not replace bytes with lossy strings or change stable resource hashes without migration/versioning.
- Acceptance: malformed/oversize/truncated frames, unavailable inherited service, repeated requests without closing original FD, non-UTF-8 Unix names, stable identity, and wrong-authority isolation remain covered; alternate platform supplies an equally restricted transport.

### A04-03 — Files trash, known folders, and symlinks contain target policy (high)

- Evidence: `apps/file-explorer/src/fileops.rs:55` always builds `dirs::data_dir()/Trash/files`; `:95`, `:106`, `:117`, `:138`, `:153` choose freedesktop trash enumeration/`.trashinfo` matching using `not(macos)`; `:244` delegates deletion to `trash`; `:330` Unix symlink copy with unsupported non-Unix branch; `:350`, `:356`, `:362` explicit macOS unavailable implementations. `apps/file-explorer/src/templates.rs:14` uses XDG_TEMPLATES_DIR then `~/Templates`.
- Impact: native deletion and displayed trash are not necessarily the same location (macOS has explicit unsupported management but still XDG-style browsing); `not(macos)` conflates all other targets with freedesktop semantics.
- Boundary/migration: `Trash` capability returning opaque trash-item identifiers, original location and supported operations, plus `KnownFolders` and safe symlink-copy capability. Reuse trash crate internally when qualified; keep real capability absence. Replace UI path matching with backend handles; preserve no-overwrite and no-dereference safety.
- Acceptance: delete/list/restore/purge and undo refer to the same native item; missing enumeration disables relevant actions; no fake trash directory is offered; symlink escape and recursive-self-copy tests remain production-path tests on supported targets.

### A04-04 — Files launch integration and drag handoff assume the Tilo host layout (medium)

- Evidence: `apps/file-explorer/src/files_app.rs:4435`, `:4448`, `:4457` implements CommandRunner using direct subprocess spawn and `open::that`; `apps/file-explorer/src/external_drag.rs:1`, `:33` uses a process-global drag slot explicitly intended for a compositor embedding Files in the same process.
- Condition: live file-open/terminal actions and foreign-client drags. `open` itself is a portable library; the extraction issue is that the app chooses host launch transport and the drag handoff assumes co-location, not that all process use is platform-specific.
- Boundary/migration: retain the existing CommandRunner testing seam but provide a typed platform launcher (open document/open terminal at directory with authority) and host drag-session capability. Keep app intent and drag content in Files. Review action call sites when replacing runner rather than exposing arbitrary shell strings.
- Acceptance: same resource actions dispatch to the host broker; chosen directory and file grant survive launch; unsuccessful/unsupported handoff leaves Files responsible for completion, accepted handoff suppresses duplicate file operation.

### A04-05 — Workspace and user-directory defaults are duplicated in apps (medium)

- Evidence: `apps/file-explorer/src/model.rs:8`, `:1212`, `:1230`, `:1237`, `:1525`, `:1559` handles `/var/workspace`, colon-separated roots, legacy `/workspace`, and `ui::sysdirs::user_home_override()`; `apps/disk-usage/src/app.rs:81` reads raw HOME with `/` fallback and first colon-separated workspace root with `/var/workspace` fallback. `apps/backups/src/app.rs:443` and `apps/vpn/src/local.rs:20` manually choose XDG_STATE_HOME or HOME/.local/state.
- Impact: different apps can show different homes inside a sandbox and platform-local path layout is duplicated. Disk Usage fallback can name `/` when HOME is absent; this is a source observation, not evidence of unauthorized access.
- Boundary/migration: host-provided granted places/workspace roots plus platform state/config/cache directories. Reuse existing sysdirs/settings-store semantics where available through supported public API; resolve no-home to unavailable rather than inventing a host root. Preserve configured-empty roots semantics.
- Acceptance: absent HOME, sandbox overridden home, multiple/empty workspace roots, non-UTF-8 paths and non-XDG host layout produce consistent places; no default silently grants access.

### A04-06 — OSK has an unguarded Wayland/Linux injection backend (high)

- Evidence: `apps/osk/src/inject.rs:235`, `:246` chooses and declares Wayland backend; `:400` calls `libc::memfd_create(... MFD_CLOEXEC)` for keymap; `apps/osk/src/imclient.rs:206`, `:213` selects/declares Wayland input-method-v2. `apps/osk/Cargo.toml:45`, `:46`, `:48` unconditional Wayland/libc dependencies; `:54` native xkbcommon dev dependency.
- Condition: Linux Wayland virtual keyboard + input method connection. Runtime missing-protocol fallback (`NullInjector`/None) does not make Linux FFI compile-time portable. No compilation result claimed.
- Classification: intentional backend placed in an app; extract backend, retain protocol behavior.
- Boundary/migration: existing `TextInjector` and input-context transition model are useful seams. Move virtual-keyboard/input-method dispatch, registry/seat handling, memfd and XKB keymap delivery into platform backend. Expose availability and text-context events, not Wayland proxy types, to OSK. Target-scope deps and native tests. Preserve compositor's private, credential-gated OSK connection.
- Acceptance: Linux input commit/delete/key behavior and purpose/visibility transitions unchanged; unsupported host reports no injection; app compiles without Wayland/libc memfd imports; keymap native tests run on appropriate backend runner.

### A04-07 — OSK native swipe model asset discovery is host-specific (low)

- Evidence: `apps/osk/src/futo.rs:17` depends on the existing `swipe::{FutoSwipe, ModelPaths}` FFI wrapper; `:87` searches `/usr/share/tilo/swipe-models`; `apps/osk/src/widget.rs:63` selects FUTO then geometric fallback. Dependency `apps/osk/Cargo.toml:32`.
- Boundary/migration: retain `SwipeModel` and the shared swipe wrapper; move installed-model asset discovery to a platform/runtime asset resolver, preserving explicit overrides and unavailable fallback. Native inference internals belong to the adjacent swipe audit, not here.
- Acceptance: model-present and model-absent paths are observable; packaged Linux models resolve identically; alternate hosts don't require `/usr/share`.

### A04-08 — Disks contains Linux block-device discovery and privileged operations (high)

- Evidence: `apps/disks/src/probe.rs:26`, `:37`, `:51`, `:180` invokes canonical `/usr/bin`/`/bin`/sbin `lsblk` and `smartctl`; `apps/disks/src/ops.rs:46`, `:127`, `:199`, `:237` owns UDisks2 D-Bus, udisksctl/mount/umount/mkfs/eject fallbacks, and direct `/dev/<leaf>` benchmark. `apps/disks/src/model.rs:213` defines `/`, `/sysroot`, `/home`, `/var/home` partition roles; `apps/disks/src/app.rs:1178`, `:1258`, `:1369`, `:3137`, `:3213`, `:3383` builds device display/operation paths from kernel leaf names. `apps/disks/Cargo.toml:24` enables `live` by default; D-Bus is feature guarded, not target guarded.
- Condition: reads are best-effort live probes; operations documented/runtime-gated by DISKS_LIVE. This is deliberate Linux storage administration; preserve gates.
- Boundary/migration: device inventory/health and privileged storage operations returning opaque stable device handles, display identifiers, mount roles and supported formats. Move parsers and Linux plans together; never reconstitute a target solely from app-visible `/dev` leaf. Keep authorization and fresh inventory validation in production path.
- Acceptance: read-only inventory, missing tools, denied bus, disappeared device, stale handle, format confirmation, eject eligibility, and benchmark failures tested; no backend mutation on shots; app no longer builds Linux device paths.

### A04-09 — Firewall's firewalld backend is app-local (medium)

- Evidence: `apps/firewall/src/backend.rs:80` selects live only for command availability plus TILO_FIREWALL_APPLY=1; `:102`, `:118`, `:165`, `:176`, `:205` plans/applies/probes firewalld. `apps/firewall/src/model.rs` represents firewalld zone/rich-rule concepts consumed by this adapter.
- Classification: intentional target backend to relocate, with backend-specific domain features retained as capabilities rather than falsely promising equivalent semantics everywhere.
- Boundary/migration: firewall snapshot + validate/preview/apply capability; move argv plans, permanent/reload behavior, tool detection and error classification into Linux provider. Preserve exact argument boundaries, explicit apply gate, and dry-run versus live distinction.
- Acceptance: captured-parser and argv tests move with backend; app fake tests cover unavailable, preview-only, successful and partial failed apply without fabricating host success; unsupported firewall features are visibly unavailable.

### A04-10 — Logs owns journalctl execution/parsing (medium)

- Evidence: `apps/logs/src/journal.rs:382`, `:411`, `:417` typed safe errors and bounded `journalctl -o json`; Unix OsString test construction at `:756`. `apps/logs/tilo-app.toml:3` describes host journal access (its comment about sample fallback must not be used as authority over current implementation).
- Boundary/migration: `SystemLogs` query provider returning normalized LogEntry and safe classified status; move journal-format parser and Linux invocation together. Keep sample data separate and preserve sanitization: raw stderr can expose machine context.
- Acceptance: missing/denied/failed/empty remain distinct; output/time bounds unchanged; fixture parsing validates journal backend; app tests consume the same normalized provider interface.

### A04-11 — Monitor mixes portable sampling with Linux enrichment and POSIX termination (medium)

- Evidence: `apps/monitor/src/sampler.rs:13`, `:59`, `:88`, `:144` uses sysinfo to collect local process/system/network data and Signal::Term/Kill; `:294` reads `/proc` to classify source availability; `:329` counts `/proc/<pid>/fd`; non-Linux branches at `:299`, `:336` are explicit. Dependency `apps/monitor/Cargo.toml:33`.
- Impact: existing cfgs prevent those direct proc reads on other targets, but native collection/error semantics and signal behavior remain in app. Non-Linux process source probe always succeeds, limiting distinction between unavailable and empty.
- Boundary/migration: process telemetry provider with availability and optional enrichment plus terminate/force-terminate by identity `(pid, start_time)`. Reuse sysinfo internally and preserve existing process-reuse protection, not just numeric PID. Keep chart/delta presentation in app.
- Acceptance: PID reuse rejects signaling; missing per-process permissions yields unknown optional fields; provider unavailability differs from no processes; inert/sample constructor does not probe native host.

### A04-12 — Containers owns Podman/Distrobox backend and shell launcher scripts (high)

- Evidence: `apps/containers/src/podman.rs:465`, `:590`, `:609`, `:641`, `:657`, `:688`, `:784` executes engine actions/pulls/logs; discovery also invokes distrobox. `apps/containers/src/terminal.rs:25`, `:35`, `:66`, `:81`, `:95`, `:111` constructs sh scripts, `/bin/bash` fallback, chmod 0755, XDG_RUNTIME_DIR/tmp launcher directory and terminal SHELL override.
- Condition: intentional Podman/Distrobox integration (Podman itself can exist on other hosts; rootless socket and distrobox assumptions are Linux deployment specific). Executable-bit handling is Unix guarded; non-Unix no-op does not make a sh launcher portable.
- Boundary/migration: container engine capability for inventory/lifecycle/images/logs and terminal-session capability accepting argv + execution context. Move Podman parsers/commands into provider and replace app-generated scripts with typed terminal intent. Preserve bounded-command cancellation/output behavior and existing argument quoting during transition.
- Acceptance: missing engine returns explicit unavailable rather than assumed mutation success (`run_action` currently returns Ok when absent); engine command fixtures, cancellation and terminal launch args tested; no app writes shell scripts or handles executable bits/runtime socket layout.

### A04-13 — VPN telemetry and local profile persistence retain Linux assumptions (medium)

- Evidence: `apps/vpn/src/net.rs:25`, `:109`, `:127`, `:142`, `:162`, `:174` invokes wg/ip, uses Linux IFNAMSIZ-style validation and sysfs counters including tun0/tun1 fallback; `apps/vpn/src/local.rs:20`, `:62`, `:73` uses XDG paths and guarded Unix 0700/0600 permissions. `apps/vpn/src/config.rs:260`, `:266`, `:297` contains wg-quick/OpenVPN plans; critically `:406` execution accepts only exact nmcli UUID connect/disconnect and delegates to os-actions, so the other preview plans are not evidence of reachable arbitrary execution. `apps/vpn/src/app.rs:629`, `:1046`, `:3282` already uses os-actions for NetworkManager.
- Boundary/migration: VPN provider owns exact profile-to-interface identity, counters and status; reuse os-actions for mutations. Platform private-state store owns directory resolution, private file creation and durable replacement. Keep UUID association local, never CRDT/secret-bearing export. Native plan previews belong in provider if retained.
- Acceptance: polling is tied to exact profile/interface rather than an unrelated tun fallback; unknown counters stay unknown; unavailable vs disconnected distinguished; owner-private atomic persistence validated on supported OS and existing reconnect behavior preserved.

### A04-14 — Settings embeds host identity, account, security and hardware discovery (high)

- Evidence: `apps/settings/src/panes/about.rs:46` reads os-release/proc kernel/uptime/hostname/cpuinfo/meminfo; `apps/settings/src/panes/users.rs:145`, `:176` parses passwd/group and USER with UID range 1000..60000; `apps/settings/src/panes/permissions.rs:46`, `:57`, `:71`, `:93` executes findmnt/lsblk/firewall-cmd/nft and parses EFI variable bytes; `apps/settings/src/panes/agent_models.rs:289` reads MemTotal, defaults to 8 GiB for recommendation fallback.
- Condition: live pane construction; direct Linux/Fedora inventory policy with best-effort defaults. No target guard observed around these readers.
- Impact: ported app can show incomplete/misclassified facts; e.g. unsupported firewall probe ends as “Not active”, and absent hardware memory becomes a guessed recommendation input.
- Boundary/migration: semantic system facts, device security posture, hardware capacity, and local-user directory capabilities. Linux readers/parsers move to backend; expose Unknown/Unavailable/Denied separately from a negative fact. Keep username/account UI and existing os-actions account mutations. Use opaque user identity rather than exporting Linux UID policy to generic app.
- Acceptance: provider failure never asserts disabled firewall or measured RAM; tests cover missing files, inaccessible EFI, alternate account identifier conventions and empty inventory; Linux source parsers remain fixture-tested.

### A04-15 — Settings audio extends os-actions with direct wpctl logic (medium)

- Evidence: `apps/settings/src/panes/audio.rs:145`, `:172`, `:193`, `:310`, `:332`, `:357` defines source/device control, PipeWire numeric device IDs, parses wpctl status, sets default/input gain, and probes default source. This coexists with os-actions Dispatcher and media graph dependency (`apps/settings/Cargo.toml:69`).
- Boundary/migration: extend shared audio capability (inventory, default input/output, gain/mute) using existing existing `InputControl` (`:148`) and `DeviceControl` (`:328`) seams; WirePlumber/wpctl implementation and parser belong with platform audio backend. Expose native IDs only as opaque provider handles.
- Acceptance: existing injected controls and parser fixtures port to new interface; absent backend reports unavailable without fake hardware; UI change guard avoids writes during initial synchronization; audio graph and default-device selection remain consistent.

### A04-16 — Settings input-source/timezone catalogs and fallback paths are native (medium)

- Evidence: `apps/settings/src/input_sources.rs:25` reads XKB evdev/base XML under /usr/share/X11; `apps/settings/src/panes/time_date.rs:95` loads installed zone tables; `:670` reads /etc/timezone and /etc/localtime symlink and falls back to Europe/Amsterdam. Actual time/NTP/timezone mutations already use os-actions (`:636`, `:645`, `:748`).
- Boundary/migration: keyboard/input-source catalog and timezone catalog/current-zone providers. Move XKB and zoneinfo parsers/path resolution to backend; preserve standard IANA timezone domain identifiers. Existing os-actions setters remain one authority. A fixed test zone can remain in tests; unsupported current-zone discovery should be explicit.
- Acceptance: Linux layout/variant ordering and timezone map unchanged; missing catalog gives unavailable/empty rather than silently asserting installed layouts; current zone is accurate or unknown, independent of local filesystem layout.

### A04-17 — Settings embeds Bootc/systemd deployment management (high)

- Evidence: `apps/settings/src/panes/updates.rs:76`, `:119`, `:141`, `:213`, `:240`, `:385`, `:390`, `:416`, `:462`, `:482`, `:495`, `:504`, `:557`, `:621` owns bootc status parsing, systemctl timers/services/reboot, /var/lib/tilo/update-policy and os-release. Existing `UpdateControl` at `:289` and `Rebooter` at `:274` are explicit seams.
- Classification: intentional Fedora Bootc backend, not generic update policy to impose on another host.
- Boundary/migration: move RealUpdateControl/RealRebooter and live UpdateStatus gathering plus Bootc parsing to deployment provider. Keep staged/rollback/confirm/policy model and recovery UX in app; keep fixed privileged services, no generic privileged command endpoint. Firmware operations already delegate to os-actions.
- Acceptance: current/staged/rollback state, policy apply, firmware status, command timeout, denied polkit and reboot errors match existing behavior; unsupported host exposes unavailable control rather than starting Linux tools; retain exact service authorization boundaries.

### A04-18 — Settings device mesh and local model admin owns Linux helper locations/processes (high)

- Evidence: `apps/settings/src/panes/device_mesh.rs:28`, `:32`, `:33`, `:474`, `:1438`, `:1457`, `:1488`, `:1514`, `:1585`, `:1625` assumes /etc relay config, /var/lib/tilo identity storage, /usr/libexec home-peer-control, /usr/bin enroll/relay-admin, pkexec and AsyncCommand queues. `apps/settings/src/panes/agent_models.rs:33`, `:207`, `:229` provides ModelManagerCli and existing LocalModelControl.
- Boundary/migration: typed device-host admin and local-model management capabilities; backend owns helper resolution, process lifecycle and authorized privilege elevation. Reuse LocalModelControl and existing tilo-relay domain/config types; don't clone daemon business logic into platform. Device mesh model/UI stays app-side.
- Acceptance: same bounded helper argument vocabulary, timeout/reaping, cancellation and partial status; no secrets exposed in shared UI state; missing helper becomes unavailable; model admin fake tests use production interface.

### A04-19 — Store has host Flatpak probes outside its existing app-host broker (medium)

- Evidence: `apps/store/src/flathub_client.rs:353`, `:376`, `:382`, `:390` executes flatpak list/version/remotes; `apps/store/src/app.rs:1047` probes availability on construction; `apps/store/src/worker.rs:256`, `:277`, `:297`, `:651` probes remotes. In contrast `apps/store/src/worker.rs:172` connects app-host from service and `apps/store/src/actions.rs:174`, `:181`, `:187` already delegates Flatpak mutations there. `apps/store/src/installed.rs:27` resolves the Tilo registry through dirs::data_dir.
- Classification: read-only discovery transport leakage; Flathub HTTP/catalogue and .flatpakref parsing are portable product integrations and should remain outside generic OS code.
- Boundary/migration: extend app-host/platform package-host snapshot to expose availability, remotes, installed packages (only migrate installed_flatpaks if reachable/retained; lexical presence alone doesn't prove a caller). Make broker snapshot authoritative rather than app process PATH. Let provider resolve native registry location if that state remains host-owned.
- Acceptance: read and mutation use same host context and remote identities, unavailable not “none installed”, time/output bounds retained, no direct flatpak execution in app; registry/trust behavior unchanged.

### A04-20 — Windows app owns Linux appliance helpers and Unix service handle adoption (high)

- Evidence: `apps/windows/src/appliance_owner.rs:13`, `:14`, `:358`, `:410`, `:440`, `:470`, `:489`, `:494`, `:598` resolves /usr/libexec helper/console, pkexec and bounded execution; `apps/windows/src/provider.rs:4`, `:462`, `:468` adopts launcher-owned compat FD as OwnedFd and SessionClient. This is the app for Windows compatibility on Tilo, **not proof of a Windows-native host backend**.
- Boundary/migration: appliance-owner capability for status/import/lifecycle/recovery and compatibility service connector taking verified opaque service handle. Keep provider/action state machine; move helper/FD transport into platform/runtime integration next to existing compat-service types. ISO must remain an already-open granted file passed via stdin; do not replace with a privileged user-controlled path.
- Acceptance: missing service/helper/authorization classified; bounded process output/time and busy state preserved; FD adopted exactly once; lifecycle whitelist and portal-file authority maintained. Native compatibility implementation belongs to adjacent crates/windows audit.

### A04-21 — Backup metadata and private temporary output use Unix-specific primitives (medium)

- Evidence: `apps/backups/src/engine.rs:76` conditionally includes mode in BackupEntry; `:203`, `:487` captures/restores Unix permission modes; `:718`, `:724` creates symlink or reports unsupported. `apps/print-centre/src/surface.rs:5`, `:362` uses guarded Unix OpenOptionsExt mode(0600) for local scan output; print/scan operations themselves already delegate to os-actions (`:2446`, `:2989`).
- Impact: backup wire shape/metadata depends on compilation platform and generic restore must not interpret foreign permission bits as universal semantics. Private output security needs an explicit equivalent on non-Unix hosts; absence of chmod isn't evidence of an actual leak on those hosts.
- Boundary/migration: platform filesystem metadata/copy-link/private-file capability; retain backup traversal/hash/integrity algorithm and scan/document UI. Define versioned optional native metadata plus portable kind/read-only attributes rather than target-conditioned schema. Reuse shared private atomic-file functionality where contract fits.
- Acceptance: cross-host manifest round-trip retains unknown metadata, Unix mode restore unchanged, symlinks never followed unexpectedly, unsupported link restoration errors honestly, temporary scan output satisfies platform owner-private contract.

## Intentional delegated backends, packaging and tests

### A04-22 — Preserve existing shared boundaries; target-scope deployment and native certification (medium migration coordination)

This grouped item is coordination/retention work, not an assertion that every listed import needs extraction.

- `apps/settings/src/dispatcher.rs:12` already uses `os_actions::{Dispatcher, RealDispatcher}`. Hardware operations in `panes/network.rs:27`, `panes/display.rs:44`, `panes/power.rs:38`, `panes/printers.rs:105`, `panes/bluetooth.rs`, `panes/notifications.rs`, `settings_app.rs`, and `actions.rs` largely consume shared services. Review them as callers when shared system/media backends move, not as duplicate backend implementations. Display discovery at `panes/display.rs:247` already loads `DisplayHardwareSnapshot`; its preceding `/sys/class/drm` comment is stale and is not evidence of a direct sysfs reader. Direct `runtime_portal_xdg::FileChooser/OpenUri` at `panes/network.rs:2538`, `:2558` is a concrete platform-specific dependency to route through the chosen neutral portal capability.
- `apps/settings/src/panes/storage.rs:9` already uses runtime-fs storage; `apps/settings/src/accounts.rs:863`, `prefs.rs`, `region.rs` and `apps/themes/src/store.rs` already share settings-store. `apps/themes/Cargo.toml:22` confirms that dependency. Themes' JSON customization model is portable; no native theme backend should be invented in Themes. The adjacent settings-store audit owns directory/native effects. `dirs` config/cache use in Files/Store is itself portable; only host-owned state placement and ad hoc defaults warrant consolidation.
- All 15 `tilo-app.toml` files deliberately declare Linux namespace/seccomp/Landlock packaging; all except Store also declare x86_64 v3 ISA. Keep these as Linux deployment manifests or generate target profiles from semantic capability declarations. Do **not** weaken policies globally for portability. Files/Settings AT-SPI acceptance entries are Linux release certification, not cross-platform UI business logic.
- Native tests: Files `tests/live_collaborative_mount.rs:21` requires /dev/fuse, but currently returns early when absent unless TILO_FS_FUSE_REQUIRED is set (`:23`). Preserve a mandatory Linux certification runner and make absence fail there; do not count that early return as portability success. Files xattr fixtures span `src/model.rs`, `src/framework_history.rs`, `src/history_control.rs`, `src/files_app.rs`, `src/views/file_view.rs`, and `src/main.rs` shot seeding. Unix sockets in Files sharing/received-share tests, symlink tests in fileops/archive/native_proof and backups, and OSK xkbcommon tests belong with their native adapter or explicit backend suite.
- `apps/file-explorer/tests/atspi_smoke.sh:7` uses busctl/gdbus and a real Linux accessibility bus. `apps/store/verify-open-flow.sh:1` is a Bash developer integration script with Python HTTP server and signals. Retain platform-specific tooling; do not mechanically rewrite shell syntax into generic app APIs. `apps/osk/data/generate.py` generates language data, not an OS runtime backend. Ordinary Command::new(current_exe) test process isolation and shots binary execution are not native leaks by themselves.
- Existing XDG-dependent tests in `apps/settings/tests/prefs_persistence.rs`, `tests/accounts_mail_loop.rs`, `src/prefs.rs`, `apps/themes/src/app.rs`, `src/framework_history_tests.rs` should inject directory provider state after migration, retaining Linux XDG tests inside provider. Settings device_mesh tests launch sleep/true; Windows appliance_owner tests launch sh; VPN net tests inspect loopback/ip. Keep native execution tests explicit and add portable provider tests rather than ignoring failures.

Migration acceptance: Linux package sandbox and privileged effects remain equally confined; alternate package profiles declare only supported capabilities; mandatory native checks fail on missing prerequisites; portable app tests exercise fake capability contracts through real application dispatch rather than tautological helper assertions.

## Coverage appendix and exclusions

Additional inspected files whose platform-looking content is chiefly sample/test/domain text: `apps/containers/src/model.rs`, `doc.rs`, `tests/interaction.rs` (container-internal POSIX mounts are valid Linux-container data even on a non-Linux host); `apps/backups/src/model.rs`; `apps/disks/src/model.rs` sample paths in addition to live role policy identified above; `apps/logs/src/model.rs` and sample portion of `journal.rs`; `apps/monitor/src/model.rs`; `apps/disk-usage/src/app.rs` shot scenarios; Files archive safety tests and URI/domain parsers; generic `std::fs` directory walking in `apps/disk-usage/src/scan.rs`. Standard HTTP/TLS through reqwest/rustls/webpki in Settings account probing, Files WebDAV, Store registry/catalogue/image fetches is not an OS trust-store API in these sources. No per-app build.rs found. No vendor directory found in assigned scope.

For implementation ordering: extract reusable filesystem/service-handle/private-storage and system discovery contracts first; preserve shared os-actions/settings-store/app-host boundaries; then move each intentional Linux backend with its native parser/command tests; finally remove app-local cfg/native dependencies and adapt packaging/certification. This audit identifies source-level coupling, not demonstrated failures.

Summary: **22 grouped findings/coordination items**. Adjacent required audit ownership: crates/system/{os-actions,bounded-command,settings-store,atomic-file,swipe,desktop-entry,accounts,tilo-bluez}, crates/runtime/{runtime-portal,runtime-portal-xdg,runtime-fs}, fs, shell service/drag/OSK transport, crates/media/core, crates/packaging/{app-host,launcher}, crates/hosting/tilo-relay, crates/windows/compat-service and OS helper deployment. No app-framework internals audited.
