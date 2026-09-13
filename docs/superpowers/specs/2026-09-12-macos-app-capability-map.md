# macOS app capability map

Status: proposed qualification map, 2026-09-12.
Source audit: `d19f1a0ae2b1d05ec382188fd975bd5762e10704`.
Scope: all 47 `apps/*/qos-app.toml` manifests at that commit.

Inventory revalidation: `a1a0402f39530b8e409a5b3a12c00b158f0abb6c`, 2026-09-13.
Manifest tree fingerprint: `7627838e7dd384276f88375336957da714487873e01dcd32e37a83257679f6fb`
(SHA-256 of the path-sorted `git ls-tree` records for the 47 manifests).
This revalidation checks identity, role, launch-link and declared-capability
coverage only. It does not replace the original `d19f1a0` row-level source
audit or claim that later implementation changes were re-audited.
Machine-readable applicability and deliberately unrun host results are in
[the qualification record](2026-09-12-macos-app-qualification.json).

This accompanies the [portable shell design](2026-09-12-macos-shell-port-design.md).
Real Notes and Files are the first milestone. The remaining rows identify work
needed to qualify the existing apps through the same framework. This source
audit is not evidence that an app has run successfully on macOS or Linux.

## Shared qualification contract

Every row is currently **blocked pending portable-host qualification**. Notes
and Files are the two delivery targets for the first milestone. A later app is
not supported merely because it compiles, already implements `AppWidget`, or
can produce a screenshot.

GUI apps inherit the common component gate: one canonical definition identified
by the manifest package ID, used by embedded, standalone and headless runners;
scoped services and background wakes; correct close/reopen behavior; and the
applicable framework conformance checks. The portable definition is proposed
by the design, not an interface already supplied by every current app crate.

The manifest inventory also contains `dev.qos.agent`, a CLI service. Retain and
qualify its service entrypoint, broker lifecycle and authorized action interface;
do not invent a widget to include it in the GUI catalogue. `dev.qos.osk` is a
shell input surface and must retain its focus/input-method role. Its hosting
qualification is distinct from opening an ordinary user tile. Both remain in
the complete inventory with explicit applicability results.

For each app, the qualification record must name its component definition or
service entrypoint and
record results separately on macOS and Linux for:

- Its primary user flows, including required services and failures.
- Live resources/actions and their document, view or effect semantics.
- Collaboration, identity and authority; presence and explicit following.
- Document history, local View restoration and offline recovery.
- Keyboard/IME input, accessibility, theme/scale and native integration.
- The corresponding headless action and persistence scenarios where applicable.

Results are pass, fail, not run or not applicable, with evidence and a reason.
Not applicable means the app has no corresponding feature. An unavailable
provider is a blocker. Required failures and tests not run prevent a supported
claim. Optional features can remain visibly unavailable if the advertised
support scope names them explicitly and preserves their documents and state.

## Shared follow-ups

These identifiers are tracked design work, shared by the app rows below. They
refer to workstreams in the [main design](2026-09-12-macos-shell-port-design.md#10-workstreams-and-acceptance-gates).
Completing one provider should qualify its consumers without creating separate
Mac implementations of their application logic.

| Follow-up | Shared responsibility | Workstream |
| --- | --- | --- |
| H | Canonical component factories, instance lifecycle/wakes, capability context, resource/action routing and full framework conformance. | A, B, C |
| F | Durable workspace records; local file grants, operations, metadata, trash, dialogs and open-in-app routing; portable service independent of Linux FUSE. | D |
| I | Identity, host secret storage, accounts, authenticated collaboration and service authorization. | D |
| N | App-scoped network/account services, cancellation, reconnection and typed failures. Portable protocol logic remains shared. | D, E |
| W | Real web engine and native-child-view integration: tile clipping, overlays, focus, IME and accessibility. | E |
| M | Media decoding, playback, audio output/input, capture and encoding, with accurate availability and permissions. | E |
| P | Authorized process/tool launch, PTYs, lifecycle and platform paths; preserve Linux execution policy. | E |
| S | Host integration adapters. Qualify the row-specific subtype: privileged/system administration, notification, screen capture, location, presentation, clipboard/text injection, clock, media controls, theme/settings, URI opening, input surface, remote input, or host observation. Keep Linux facilities; expose Mac equivalents only where the operation has defined semantics. | E |
| K | OS/ABI-aware independent package selection, execution bridge and confinement. Separate later specification. | Later packages |

`S` is an umbrella scheduling label, not a claim that these operations share
one provider or one authority model. The structured record carries each row’s
subtype from the existing prose and distinguishes privileged/system
administration from ordinary host integration. A listed subtype remains a gap
or unknown until its host result has evidence; it never implies support.

## Qualification result record

The companion machine-readable record preserves the 47 manifest identities,
roles, launch sources, family qualification references and `S` subtypes. A
listed family or subtype identifies a boundary named by the shared contract or
existing row prose; it does not establish capability applicability or support.
Each record resolves required-category applicability through its applicability
profile and macOS/Linux results through its host-result profile. The shared
`portable_host_not_run_2026_09_13` host profile records both hosts as `not_run`
because the inventory revalidation executed no app qualification. It does not
overwrite or infer results from legacy Linux tests. `not_applicable` is used
only for the Agent service’s source-established non-graphical checks; all other
uncertain category applicability remains `unknown`.

H applies to GUI apps, with the surface-specific OSK contract above. Agent needs
the corresponding service lifecycle/authority checks, not GUI-only checks.
Other letters identify additional boundaries to qualify;
they do not imply that an entire service family must be completed to enable one
operation. The first milestone completes F and I as used by Notes and Files,
plus their required N operations, rather than postponing their core collaboration
and recovery behavior to a later release.

## Notes and Files acceptance

Notes must use the real `NotesApp`. Its existing workspace/page savers need a
shared binding to the host record service, with a single durable writer and an
idempotent import that preserves existing data. Files must use the real
`FileExplorerWidget`, with injected roots, grants and host operations. Its
open-document route must retain the requesting instance's scope.

The first milestone is complete when both real components work inside the Mac
shell and Linux shell, not just as standalone apps. Create a Notes record, find
and open it in Files, edit it in two independent tiles, invoke its actions,
collaborate between hosts, follow/unfollow, work offline, reconnect and restore
after restart. Also verify Files' granted-directory operations, denied/revoked
access, and the corresponding Fedora headless scenarios. The full walkthrough
and required host test matrix remain in the main design.

Files' service contract must preserve the result of each operation. Enumeration
and metadata, create, rename, copy, move and trash use granted resources. Name
conflicts require an explicit resolution; overwrite is not inferred from a
retry. Cross-volume moves report their copy/remove stages and do not remove a
source before its destination succeeds. Multi-item operations report per-item
completion, partial failure and cancellation; cancellation does not claim to
undo completed effects. Resolve symlinks within the granted authority and check
permission/revocation when performing each operation.

Directory changes from another app, host tool or collaborator must become
visible through a granted-root change subscription and refresh mechanism.
Coalesce invalidations, reject stale enumeration results, and stop delivering
instance events on close. Providers that cannot watch a root must expose a
defined refresh path and refresh on reactivation, rather than imply a live view.

Internal file clipboard and drag payloads carry resource handles plus copy/cut
intent. The host translates native file URLs at the OS boundary and obtains the
appropriate grants; a path string is not authority. Clipboard payload ownership
belongs to the host, not a callback into a closed widget. Revalidate access when
the payload is used, finish a cut only after the move succeeds, and report stale
or revoked grants without losing the source. Linux keeps its provider behind
the same contract.

## Complete manifest inventory

Each row is blocked by the shared qualification requirements above, plus its
listed service gaps. The first flow identifies a qualification scenario, not a
claim that all of its features already exist. “Verified” below means visible in
source or a manifest; runtime behavior still needs the host-specific results.
A manifest declaration alone does not establish a working implementation.

The manifest link identifies the package; each launch-source link locates the
existing entrypoint to extract or adapt. Forty-six GUI entries use `LaunchSpec`;
Agent uses its service entrypoint. None of these launch links is a claim that
the proposed portable factory already exists.

| App ID | First flow to qualify | Additional families and current known gap |
| --- | --- | --- |
| [dev.qos.agent](../../../apps/agent/qos-app.toml) · [launch](../../../apps/agent/src/main.rs) | Join a document, plan against exported actions, and apply an authorized edit. | **I/N:** manifest requires collaboration and model-gateway; source calls `qos_model_gateway`. This is a non-window service: qualify its existing service entrypoint and broker/action lifecycle, with GUI checks not applicable. Cross-host broker/session behavior is unknown. |
| [dev.qos.api-tester](../../../apps/api_tester/qos-app.toml) · [launch](../../../apps/api_tester/src/main.rs) | Send a user-authored HTTP request and inspect/save the response. | **N** verified through unconditional `reqwest` and manifest host egress. **F** is only manifest-declared through optional `qos-fs`; concrete save/import requirements were not established. Needs a scoped HTTP provider or an explicitly authorized direct-network component contract. |
| [dev.qos.backups](../../../apps/backups/qos-app.toml) · [launch](../../../apps/backups/src/main.rs) | Select source/repository/restore directories, create a backup, then restore. | **F** verified through file-chooser code and manifest. Needs durable directory grants plus long-running file-operation progress, cancellation, and recovery semantics. Backup-engine portability is unknown. |
| [dev.qos.browser](../../../apps/browser/qos-app.toml) · [launch](../../../apps/browser/src/main.rs) | Load and interact with a real HTTPS page in an embedded tile. | **W/N** verified: `BrowserWidget` uses `ui::webview::WebViewEngine`, `reqwest` fetches filter lists, and manifest permits host egress. Current design explicitly says the deterministic engine is insufficient; a macOS engine/native-child-view provider with tile clipping, focus, IME, accessibility, capture, and lifecycle is missing. **F** is needed for bookmarks/cache and granted local opens. |
| [dev.qos.calculator](../../../apps/calculator/qos-app.toml) · [launch](../../../apps/calculator/src/main.rs) | Enter/edit an expression and graph/evaluate it with undo/restoration. | No extra host family verified beyond H and document/View conformance; manifest says no file, socket, or exec. Whether all graph rendering paths are target-portable is unknown. |
| [dev.qos.calendar](../../../apps/calendar/qos-app.toml) · [launch](../../../apps/calendar/src/main.rs) | Configure a CalDAV account, sync, and import/export an ICS event. | **N/I/F** verified by `caldav`, account/store code, optional live `reqwest`, and manifest comments. Needs portable account/secret, network, and granted-file providers; notification/reminder delivery was not established by this audit. |
| [dev.qos.camera](../../../apps/camera/qos-app.toml) · [launch](../../../apps/camera/src/main.rs) | Obtain camera permission, show preview, capture, and save/open a scan result. | **M/F/S** verified: source has capture/video/media/library paths; live capture uses Linux V4L2/GStreamer, manifest declares camera-device authority and host URI opening. Needs macOS camera/capture/permission and media providers plus granted export/open routing. Default-build real capture status is explicitly unknown. |
| [dev.qos.chat](../../../apps/chat/qos-app.toml) · [launch](../../../apps/chat/src/main.rs) | Send messages and attachments, then make an audio/video call and share the screen. | **N/M/F/I/S** verified: live media uses iroh, GStreamer, `ashpd` ScreenCast/PipeWire, permissions, attachments, and shell UI. Needs portable peer transport bindings, camera/mic/audio/video, macOS screen-capture provider, notifications/identity, and granted attachments. Live-media is feature-gated, so current product availability is unknown. |
| [dev.qos.code-editor](../../../apps/code_editor/qos-app.toml) · [launch](../../../apps/code_editor/src/main.rs) | Open a project, edit/save a file, start its terminal, and invoke an LSP. | **F/P** verified by portal/disk code and manifest’s `$SHELL`, PTY, and `QOS_CODE_LSP` requirements. Needs granted project-tree operations plus shared PTY/process/tool-launch and LSP lifecycle services. Source notes its terminal belongs in a shared terminal widget; portable component integration is missing. |
| [dev.qos.contacts](../../../apps/contacts/qos-app.toml) · [launch](../../../apps/contacts/src/main.rs) | Import a vCard, edit a contact, export it, and deliver a reminder/notification. | **F/S** declared by file-chooser and notifications portals; vCard/storage source confirms import/export domain. Needs granted import/export and native notification providers. Notification behavior and host-contact integration, if any, are unknown. |
| [dev.qos.containers](../../../apps/containers/qos-app.toml) · [launch](../../../apps/containers/src/main.rs) | List a real rootless engine, start/stop a container, view logs, and attach a shell. | **S/P** verified: source shells out to `podman`/`distrobox`, polls them, and launches Terminal; manifest names the Podman user socket. Needs a typed container-engine/process/terminal service. macOS Podman-machine semantics are unknown and must not be inferred from Linux CLI behavior. |
| [dev.qos.db-client](../../../apps/db_client/qos-app.toml) · [launch](../../../apps/db_client/src/main.rs) | Connect, query, inspect results, and export CSV. | **F** verified for save portal and SQLite. Manifest states SQLite is in-memory and PostgreSQL/MySQL rows are inert with no linked driver, so **N/I** are future gaps rather than current working features. Do not qualify remote databases until real drivers, network authority, and credential custody exist. |
| [dev.qos.diffmerge](../../../apps/diffmerge/qos-app.toml) · [launch](../../../apps/diffmerge/src/main.rs) | Pick two files and compare/merge without ambient file access. | **F** verified through `ui::portal` and direct reads of returned paths. Needs the portable granted-handle chooser/read contract; current portal path behavior is Linux-oriented. Write-back/merge destination behavior is unknown. |
| [dev.qos.disks](../../../apps/disks/qos-app.toml) · [launch](../../../apps/disks/src/main.rs) | Inspect real disks/SMART data and perform one authorized mount/format operation. | **S/P** verified: UDisks2 D-Bus plus `lsblk`, `smartctl`, `udisksctl`, `mount`, `mkfs`, and direct block-device access. Needs a privileged typed disk service with macOS implementation or explicit unsupported results. Current live path is Linux-specific and feature/runtime gated. |
| [dev.qos.ebook](../../../apps/ebook/qos-app.toml) · [launch](../../../apps/ebook/src/main.rs) | Open an EPUB from a granted library, paginate it, annotate, and restore progress. | **F** verified by manifest library grants and `local_library`/`epub` source. Needs granted directory/document handles and record/View separation for annotations versus reading position. Codec/renderer portability is unknown. |
| [dev.qos.feeds](../../../apps/feeds/qos-app.toml) · [launch](../../../apps/feeds/src/main.rs) | Add an HTTPS or Gemini feed, refresh it, import/export OPML, and open an article link. | **N/F** verified: `reqwest`, raw Gemini TLS socket, file chooser, direct OPML file IO, and `xdg-open`. Needs scoped HTTP/Gemini transport, granted import/export, and host URI opener; direct `xdg-open` is a verified Linux-only gap. |
| [dev.qos.file-explorer](../../../apps/file-explorer/qos-app.toml) · [launch](../../../apps/file-explorer/src/main.rs) | In a granted directory, list/create/rename/copy/move/trash files, drag/drop, and open a file in its registered app. | **F/N/I** verified: real `FileExplorerWidget`; direct `std::fs`, `trash`, ambient home/workspace-root discovery, app-local clipboard/external-drag state, DAV/sharing/network code, and host opener. It must receive roots/grants/path policy/record service from `HostContext`; file operations, file clipboard/drop, trash, external-change observation, DAV identity, and catalogue open routing need portable providers. This is a first-milestone blocker. |
| [dev.qos.firewall](../../../apps/firewall/qos-app.toml) · [launch](../../../apps/firewall/src/main.rs) | Read policy, preview a change, and apply it only after authorization. | **S/P** verified: backend invokes `firewall-cmd` behind an environment gate; manifest says it does not touch nftables directly. Needs a typed privileged firewall service or an explicit unsupported result on macOS; direct PATH/env gating is not the shared contract. |
| [dev.qos.git-client](../../../apps/git_client/qos-app.toml) · [launch](../../../apps/git_client/src/main.rs) | Open a granted repository, inspect changes, commit, fetch, and push. | **F/N/I/P** verified by `git2`, repository discovery, arbitrary remote egress, SSH agent/`~/.ssh`, and credential-helper requirements. Needs repository grants (including upward discovery policy), network transport, credential/agent custody, and authorized helper execution. macOS credential integration is unknown. |
| [dev.qos.hex-editor](../../../apps/hex_editor/qos-app.toml) · [launch](../../../apps/hex_editor/src/main.rs) | Open one granted file, inspect/search bytes, and restore the view. | **F** verified by manifest and positional launch target; app is read-only. Needs a resource/handle launch target rather than ambient argv path. No write capability is currently claimed. |
| [dev.qos.imager](../../../apps/imager/qos-app.toml) · [launch](../../../apps/imager/src/main.rs) | Pick an image, identify a target device, and write/verify only after explicit authorization. | **F/S/P** verified: manifest says sandboxed qpkg disables writing without a narrow raw-device portal, while separate host downloads contain a privileged writer; source has probe/write/release paths and optional Linux `zbus`. A cross-host raw-device service with destructive-operation confirmation/progress is missing; bundled-host versus in-shell component status is unknown. |
| [dev.qos.logs](../../../apps/logs/qos-app.toml) · [launch](../../../apps/logs/src/main.rs) | Read/search/filter live host logs and export a selection. | **S/P/F** verified: manifest and `journal.rs` use `journalctl -o json` and otherwise fall back to samples; file chooser is declared. Needs a typed host log stream and granted export provider. macOS unified-log support is absent/unknown, and sample fallback must not count as qualification. |
| [dev.qos.mail](../../../apps/mail/qos-app.toml) · [launch](../../../apps/mail/src/main.rs) | Add an account, authenticate, sync IMAP, send SMTP, open a link, and attach/save a file. | **N/I/F/S** verified: live IMAP/SMTP, OAuth loopback flow, shared account vault, direct `xdg-open`, attachments, and optional spellcheck. Needs portable network/account/Keychain/OAuth callback/opener/file providers; direct opener is a verified Linux gap. Notification delivery is not declared in the manifest and remains unknown. |
| [dev.qos.maps](../../../apps/maps/qos-app.toml) · [launch](../../../apps/maps/src/main.rs) | Search, load tiles, route, bookmark a place, and use an offline cache. | **N/F/S** verified: allowlisted OSM/Nominatim/Overpass/OSRM/Transitous endpoints, tile cache, offline panel, and shell-ui declaration. Needs scoped HTTP/cache provider and location service if current-location flows are advertised; location authority/provider status is unknown. |
| [dev.qos.monitor](../../../apps/monitor/qos-app.toml) · [launch](../../../apps/monitor/src/main.rs) | Show live process/CPU/memory data and inspect a process. | **S** verified: sampler directly reads Linux `/proc` and manifest names `/proc`/`/sys`. Needs a portable system-metrics/process inventory provider; current live sampler is Linux-specific. |
| [dev.qos.music](../../../apps/music/qos-app.toml) · [launch](../../../apps/music/src/main.rs) | Open a library or stream, play audio, seek, and expose media controls. | **M/N/F/S/I** verified: GStreamer audio, streaming/lyrics HTTP, iroh sync, MPRIS D-Bus, library files, secret portal, and `nmcli` are described by source/manifest. Needs portable playback/audio-device, streaming transport, granted library, secrets, and native media-control providers. Several live features are optional, so real default behavior is unknown. |
| [dev.qos.notes](../../../apps/notes/qos-app.toml) · [launch](../../../apps/notes/src/main.rs) | Create/edit a real note, invoke an action, collaborate Linux↔Mac, recover offline, and restore two independent views. | **F/I** plus common H verified as blockers: standalone `NotesApp` owns `Saver`/`PageSaver`; `LaunchSpec` disables framework autosave; Notes does not implement `AppWidget`. It needs a canonical factory, host record writer, versioned document/View restore, idempotent import of existing data, host identity/collaboration/presence, scoped background wakes, and accurate optional spellcheck availability. This is the other first-milestone blocker. |
| [dev.qos.osk](../../../apps/osk/qos-app.toml) · [launch](../../../apps/osk/src/main.rs) | Appear for focused text input, inject composed text/keys, and disappear with the target lifecycle. | **S** verified: manifest identifies a shell-owned input-method/virtual-keyboard surface on a private compositor display; source has `imclient`/`inject`. It should be a hosted service/surface rather than a user tile. macOS text-input injection and secure focus routing are unknown. |
| [dev.qos.pdf-viewer](../../../apps/pdf_viewer/qos-app.toml) · [launch](../../../apps/pdf_viewer/src/main.rs) | Open a granted PDF, render/search/select text, annotate, and export safely. | **F** verified by portal, direct file IO/mmap-style source handling, temporary publication, and MIME providers. Needs handle-based open/export and portable file-publication semantics; PDF rendering itself appears shared but native qualification is unknown. |
| [dev.qos.photos](../../../apps/photos/qos-app.toml) · [launch](../../../apps/photos/src/main.rs) | Pick an image, render/edit its shared state, and restore the view. | **F** verified: real `AppWidget`, chooser, and direct file read; manifest denies network/exec. Needs canonical factory plus granted-handle import. Export/library breadth was not established and remains unknown. |
| [dev.qos.remote](../../../apps/remote/qos-app.toml) · [launch](../../../apps/remote/src/main.rs) | Connect to an explicitly selected SSH/VNC or authenticated qOS target, interact, and disconnect cleanly. | **N/I/M/S** source/manifest verified at feature level: SSH, VNC/RFB, and qOS exact-target routes exist; RDP is explicitly unimplemented. Needs scoped socket transport, credential custody, remote-frame/input provider, and reconnect lifecycle. Live SSH/VNC is switch-gated and current macOS behavior is unknown. |
| [dev.qos.settings](../../../apps/settings/qos-app.toml) · [launch](../../../apps/settings/src/main.rs) | Read/change one host setting, manage model/account settings, and show denied/unavailable states accurately. | **S/P/I/N** verified: real `AppWidget`; source/manifest names `systemctl`, `localectl`, `bootc`, `nmcli`, `wpctl`, printing tools, `/proc`/`/etc`, account vault, models, model-gateway, and tools. Needs typed cross-host settings/inventory/account services; direct Linux command/file probing cannot be used on macOS. Each pane needs an applicability result rather than sample success. |
| [dev.qos.sheets](../../../apps/sheets/qos-app.toml) · [launch](../../../apps/sheets/src/main.rs) | Open/import a workbook, edit/formula/undo, collaborate, and export. | **F** verified by chooser plus CSV/XLSX/ODS codecs; optional spellcheck and collaboration declared. Needs granted import/export and host record/View lifecycle. Codec and formula portability is unknown, with no extra OS-specific dependency found. |
| [dev.qos.slides](../../../apps/slides/qos-app.toml) · [launch](../../../apps/slides/src/main.rs) | Open/import a deck, edit/collaborate, present, and export. | **F/S** verified by chooser plus PPTX/ODP/assets/presentation modules; optional spellcheck/collaboration declared. Needs granted import/export and a shell/fullscreen presentation contract. The source audit did not establish whether presentation uses a separate host window, so native presentation behavior is unknown. |
| [dev.qos.snippets](../../../apps/snippets/qos-app.toml) · [launch](../../../apps/snippets/src/main.rs) | Create/search a snippet, invoke expansion/copy, and sync it. | Manifest says snippets are CRDT-only; source has expansion/highlight/search. **F/S** may be needed for system clipboard or global text injection, but the manifest declares neither and this audit did not establish a live integration. Treat expansion outside the tile as unknown, not supported. |
| [dev.qos.store](../../../apps/store/qos-app.toml) · [launch](../../../apps/store/src/main.rs) | Browse a remote catalogue, inspect/install/update an app through the authorized package host, and export evidence. | **K/N/I/F** verified: manifest requires `app-host`, optional identity, host egress; source has registry/Flathub/moderation clients, installed state, and save portal. Needs OS/ABI-aware catalogue/package selection and app-host service. Independent macOS packages are explicitly later scope, so only bundled-catalogue/control flows can be considered for this milestone. |
| [dev.qos.terminal](../../../apps/terminal/qos-app.toml) · [launch](../../../apps/terminal/src/main.rs) | Start an authorized session, run commands, resize PTY, restore screen without replay, and open a URL. | **P/F/S** verified: real `AppWidget`; manifest requires `ush`, and source/manifest depend on PTY/session supervisor plus host opener. Needs shared PTY/process/session/opener provider, instance cancellation, and thaw/reconnect rules. The Linux `ush` guest/service is not itself a macOS provider. |
| [dev.qos.themes](../../../apps/themes/qos-app.toml) · [launch](../../../apps/themes/src/main.rs) | Edit/select a theme and observe all hosted apps update and restore it. | **S** verified by manifest: palette is OS-owned and delivered through the compositor. Needs a host theme/settings service and broadcast/update lifecycle; no direct file/network gap is declared. |
| [dev.qos.totp](../../../apps/totp/qos-app.toml) · [launch](../../../apps/totp/src/main.rs) | Import a QR seed, securely persist it, generate/reveal/copy a code, and restore metadata without syncing the secret. | **I/F/S** verified: source shells out to Linux `secret-tool`, stores secret-free CRDT metadata, uses file chooser for QR, wall clock, and copy action. Needs Keychain-backed secret custody, secure clipboard, granted QR import, and clock/lifecycle handling; direct `secret-tool` is a verified Linux gap. |
| [dev.qos.tube](../../../apps/tube/qos-app.toml) · [launch](../../../apps/tube/src/main.rs) | Search/load a real video, play/seek it, and restore without replaying network effects. | **N/M/I** verified: live stream resolution uses yt-dlp/rustypipe/Google video endpoints, media widgets, and a secret portal; default build is explicitly hermetic. Needs real network/stream resolver, secret, decode/audio/video provider, and host lifecycle. Hermetic/sample UI cannot qualify the live flow. |
| [dev.qos.typst-editor](../../../apps/typst_editor/qos-app.toml) · [launch](../../../apps/typst_editor/src/main.rs) | Open/edit a `.typ`, compile preview, and export PDF. | **F/P** verified: chooser, worker/compiler/preview modules, local compilation, document-root output, optional spellcheck. Needs granted file/output service and an explicit decision whether compilation is in-process portable computation or authorized process work; current compiler execution mode was not fully traced. |
| [dev.qos.video-player](../../../apps/video-player/qos-app.toml) · [launch](../../../apps/video-player/src/main.rs) | Open a granted movie, play/pause/seek with audio, and restore view without replay. | **M/F** declared and source-evidenced by player/search/app modules and file chooser; manifest says read-only media roots plus audio output. Needs portable decode/render/audio and handle-based file provider. The concrete backend was not established in this audit, so real format support is unknown. |
| [dev.qos.video-editor](../../../apps/video_editor/qos-app.toml) · [launch](../../../apps/video_editor/src/main.rs) | Import clips, preview/edit a timeline, and export a playable file. | **M/F** verified: GStreamer export, OpenH264 decode, GPU compositor, media/portal/project-file code, and granted source directories. Needs portable decode/encode/audio/GPU provider and granted import/export service. Feature-gated codec availability and Metal behavior are unknown. |
| [dev.qos.vpn](../../../apps/vpn/qos-app.toml) · [launch](../../../apps/vpn/src/main.rs) | Import a profile, provision/activate it, show status/throughput, then remove it safely. | **S/I/F/P** verified: source delegates to NetworkManager, runs `wg`/`ip`/`pgrep`, and reads Linux `/sys`; credentials stay local. Needs a typed host VPN/network-configuration service, secret custody, and file import. Current implementation is Linux-specific; macOS VPN support is unknown. |
| [dev.qos.weather](../../../apps/weather/qos-app.toml) · [launch](../../../apps/weather/src/main.rs) | Search a location, fetch forecast, switch units, and recover from offline state. | **N/S** verified: `reqwest` to two allowlisted Open-Meteo hosts; manifest says shared settings provide units/locale. Needs scoped HTTP and host settings/locale service. No location-device capability is declared. |
| [dev.qos.windows](../../../apps/windows/qos-app.toml) · [launch](../../../apps/windows/src/main.rs) | Import/select a Windows app, choose a supported route, launch through the compatibility service, and report unavailable routes. | **K/S/F** verified: manifest declares optional `compat`; source provider is a compatibility-service boundary, while appliance-owner code directly uses `pkexec` and shell helpers. Needs an actual cross-host compatibility provider or explicit unsupported results. Independent runner/VM/package hosting is later scope; source already marks unavailable service paths. |
| [dev.qos.word-processor](../../../apps/word-processor/qos-app.toml) · [launch](../../../apps/word-processor/src/main.rs) | Open/import a document, edit/collaborate/undo, crash-recover, and export. | **F** verified: real `AppWidget`, file chooser, room persistence/autosave, crash recovery, rich-text codecs, and optional spellcheck. Needs canonical factory, host record/View lifecycle, granted import/export, and a one-writer recovery contract. No extra OS-specific provider was established. |
