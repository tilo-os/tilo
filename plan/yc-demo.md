# YC demo — plan grounded in the code

Status of every claim below was read from the tree at `60cc7e01` (2026-08-22), not from
docs — several docs (`MISSING.md`, `docs/word-processor-gaps.md`, `docs/action-model-recipe.md`
adoption table, `plan/compositor.md` keybindings) are stale in both directions.

Storyboard asked for: *Search · multiple apps · collab · 2 users · 1 agent · one scripted task.*

---

## 0. What the code forces on the storyboard (read this first)

| Storyboard beat | Code status | What we film instead / what has to be built |
|---|---|---|
| Empty desktop, Quinten opens Files | **WORKS.** Files is compiled into the compositor (`shell/Cargo.toml:108`, `AppKind::FileExplorer`, `shell/src/shell/state.rs:306`); palette on `Super+Space` / `Super+K` / `Ctrl+K` (`shell/src/shell/overlay_host.rs:5688-5726`) | As written. |
| Quinten invites Xaver | **PARTIAL.** Palette rows "Invite peer to workspace" / "Connect to peer workspace" (`shell/src/shell/search/providers/actions.rs:72-89`); token is a signed ed25519 blob with room id + owner addr + expiry (`shell/src/network/invite.rs:106-118, 146, 212, 284`). **No Copy button, no Ctrl+V in the connect prompt** (`overlay_host.rs:1514-1523`, `:9004-9160`); the `quickdesktop://invite/…` link + Copy in Workspace Settings is a fake (`workspace_settings.rs:168`). | Film the palette flow. Build: Copy button + paste (P1). Keep the Workspace-Settings invite link off camera. |
| Quinten invites the agent | **Different mechanism.** An agent is not a peer — it is a capability principal `agent:<id>` (`crates/collab/collab-types/src/caps.rs:123, 294`), granted through the Permissions tab, and it reaches the document as a broker client. The Permissions composer can't mint an `agent:` subject yet (`workspace_settings.rs:889`). | Film: Permissions tab → grant `agent:scribe` write on `app:dev.qos.word-processor`. Build: let the composer pick an agent subject (P1), or grant via the control socket off-camera. |
| "Both join, you see their cursors" | **NOT in the real compositor.** `native.rs` has zero presence code; the broker fans `Presence` to local apps only (`shell/src/shell/collab_broker.rs:772-787`, attributed as `peer-{n}`); the desktop-level `RemoteCursorOverlay` is only mounted in tests (`shell/src/shell/remote_cursors.rs:44`). Inside `ui` apps the presence layer is complete: colored cursors + name pills, carets, selections, edge mist (`ui/src/presence/render.rs:456-491`, `geom.rs:57`). | Build (P0): bridge app presence over gossipsub. Then cursors/carets appear inside the shared word processor on both machines. Desktop-level cursors over the whole strip = optional P1. |
| Search "word processor" → not installed → installs as qpkg → opens | **BUILT 2026-08-22 (P0-c, `shell/src/shell/catalogue.rs`).** Was: NOT FOUND in the palette. A miss is a notification: "`{id}` is not installed" (`shell/src/shell/launcher.rs:137-164`). The **Store app** does download → `qos-install install` (ed25519 + BLAKE3 verify) → `qos-install run` (`apps/store/src/actions.rs:248-300`). | Build (P0): a palette row "Word Processor — not installed · Install & open" that reuses the store's `open_qos` plan. Serve a local registry on set day (`registries.toml`, `GET {base}/entries`, `apps/store/src/config.rs:28-37`). |
| "Cut to Xaver: you see the window opening there too" | **NOT FOUND.** Room replication carries identity, name, owner, capabilities — *not* strips/groups/tabs (`shell/src/shell/state.rs:1928-1935`). Nothing auto-spawns on the peer. | Xaver opens the same document himself from Files. It converges because both sessions share the `RoomId` the invite set and the launcher forwards it as `QOS_WORKSPACE_ID` (`crates/packaging/launcher/src/sandbox.rs:210`, `ui/src/collaboration.rs:963-999`). Narrate it as a feature: the app is drawn on his machine, not streamed. |
| Quinten's writing appears on Xaver's screen | **PARTIAL — broken for launched apps.** The legacy JSON broker bridges outbound deltas to libp2p (`collab_broker.rs:1017-1031`) but sandboxed apps dial the capnp broker (`QOS_SVC_COLLAB_FD`), which has no network handle at all (`collab_broker_capnp.rs:601-603, 702-707`; `grep publish_app_crdt` → 0 hits). Inbound works (shared `PeerTable`). | **Build (P0, the critical fix):** give the capnp broker the same `peer_sync` outbound publish the JSON path has. Without it nothing typed in a launched app leaves the machine. |
| Drag an image from Files into the document | **BUILT 2026-08-22** (branch `files-to-app-dnd`): the compositor promotes a Files drag to a server-side `wl_data_device` drag offering `text/uri-list` + the image bytes; the vendored winit now receives Wayland drops; the word processor inserts at the drop point. Proof: `shell/tests/clipboard_e2e.rs` (`…drops_onto_a_client`, and the `QOS_LIVE_APP_DND=1` word-processor lane). The Insert-image button remains the fallback. Was: **IMPOSSIBLE on Wayland.** The vendored winit emits `DroppedFile` on X11/macOS/Windows only — there is no data_device code under `vendor/winit/src/platform_impl/linux/wayland/`. The word processor's drop handler (`apps/word-processor/src/widget.rs:5764-5777`) can never fire; Files has no `wl_data_source` (`apps/file-explorer/src/views/file_view.rs:78-146` is in-process only). | Film: toolbar **Insert image** → the compositor's own portal file chooser (`shell/src/portal/file_chooser.rs`, wired at `shell/src/compositor/hosted.rs:403` / native) → pick `photo.png`. The image is `ScalarValue::Bytes` in the Automerge doc (`crates/doc/qdoc/src/model.rs:2005`) so it replicates exactly like text. This beat is *better* than DnD: it shows the sandbox. |
| The agent does something | **BUILT 2026-08-22 (P0-d) — `qos-agent`.** Was: no LLM anywhere; the action model is real. Word processor exports `SpliceText / SetMark / SetParagraphStyle / InsertTable / InsertImage / …` as replayable, attributed actions (`apps/word-processor/src/actions.rs:112, 494, 792`); `App::invoke_semantic_action` is proven byte-identical to the toolbar (`ui/src/app.rs:2398, 7000`); the history strip renders per-author versions (`apps/word-processor/src/history_panel.rs`, `ui/src/history_source/authors.rs`). | Build (P0): a headless `qos-agent` process that joins the document's broker route and applies actions. No window, no pointer — which is exactly the pitch. Optional: a real model call inside it. |

Everything else in the storyboard (empty desktop, strip layout, Files thumbnails + sync badges,
palette, sandbox grant, per-author history, permissions tab, revoke) works as-is.

---

## 1. Setup and recording route

**Mode.** Native compositor (the default `Mode::Native`, `shell/src/main.rs:7`), `--backend winit`
(the default) so each session is an ordinary 1600×900 window on a host desktop, recorded with
OBS / wf-recorder. Reasons: `shots --mode recording` is single-compositor with no network verbs
(`shell/src/bin/shots/scenario.rs:236-260`); QEMU `screendump` can't read a GL surface
(`os/dev/vm.sh:38-42`); `Mode::Hosted` is the legacy in-process-tiles path (no word processor, no
sandbox). If you want the "real OS" shot, boot the bootc image on two laptops and capture
HDMI — same script, same flags.

**Two machines on one LAN, one session each**, started with `--enable-peer-sync`
(`shell/src/compositor/mod.rs:97-111`). Peers find each other by mDNS and the token also carries
`owner_addr` (`invite.rs:106-118`). **Nobody has ever run this across two machines**
(`MISSING.md:66`) — a full LAN rehearsal is the first milestone, before any UI polish.

**Fallback rig:** both sessions on one box, exactly as `scripts/two-session-collab-demo.py`
does it (separate `XDG_RUNTIME_DIR`/`XDG_*_HOME`, `QOS_USER_ID`, `QOS_PEER_KEY_PATH`,
`QOS_TEST_CONTROL`, lines 113-146) — drop `--backend headless` to get windows. That script
already drives `/invite` → `/connect` → rename → grant → revoke end-to-end with ~45
assertions; it is the regression test for beats 2 and 6.

**Debug build** on set: the `QOS_TEST_CONTROL` socket is compiled out of release
(`shell/src/compositor/dev_env.rs:40`). Use `shell/src/bin/qos-control.rs` or
`scripts/headless-examples/qos_client.py` to inject the token / snapshot / grant.

**Pre-staged state**
- `/var/workspace/Demo/` on Quinten's machine: `brief.docx` (or `.odt`), `photo.png`, two or
  three other files so the Files view has thumbnails and `Projected`/`Local` sync badges
  (`apps/file-explorer/src/model.rs:20-63`).
- A local registry: static server answering `GET /entries` with one entry
  (`dev.qos.word-processor`, download URL to the `.qpkg` built with `pkg-cli`), and
  `~/.config/qos/…/registries.toml` pointing at it. Note the store passes
  `--allow-self-signed --force` to `qos-install`.
- Identities: `QOS_USER_ID=quinten` / `xaver`; register the agent's handle so history shows
  "Scribe" rather than "Unknown participant" (`ui/src/history_source/authors.rs:44, 63`).
- Word processor packed + *not* installed on Quinten's machine; installed on Xaver's ahead of
  time (or let him run the same install beat off-camera).

---

## 2. The script — beat by beat

Target length 4½–5 minutes. **Actions** = what is on screen and who does it.
**Said** = the voice-over, restricted to what the code does.

### Beat 0 — Cold open (0:00–0:15) · Quinten's screen

**Actions.** Empty desktop: gradient-and-grid backdrop, 40 px top bar with the overview glyph,
workspace chevrons and the workspace name "Home" on the left, clock in the centre, an empty
avatar stack and the quick-settings gear on the right; the wheel disc on the left edge
(`shell/src/shell/desktop_backdrop.rs`, `topbar_chrome.rs:886-1000`, `sidebar_wheel_disc.rs`).
No icons, no dock.

**Said.** "This is qOS. There is no desktop and no dock. A workspace is a horizontal strip of
windows you scroll along, Niri-style; workspaces stack vertically, that disc on the left switches
between them. Each workspace is its own sandbox and its own set of collaborators."

### Beat 1 — Files (0:15–0:40) · Quinten

**Actions.** `Super+Space` → palette. Types `files` → the *Applications* row "Files" → Enter.
Files fills the strip. Navigates to `workspace › Demo`: image thumbnails, a cloud emblem on the
workspace place, `Local`/`Projected` badges on entries (`places_sidebar.rs:128`, `model.rs:20-63`).

**Said.** "Everything starts from search. `Super+Space` is machine-wide, `Ctrl+K` is scoped to
the app you're in. This folder is the workspace's shared directory — a FUSE mount. What you
see immediately is metadata that arrived over gossip; bytes are fetched the first time
something is opened."

### Beat 2 — Invite (0:40–1:25) · Quinten, then Xaver

**Actions (Quinten).** `Super+Space` → "Invite peer to workspace" → the peer-sync card shows the
token (`overlay_host.rs:9004`). Clicks *Copy* (P1 build) and sends it to Xaver.
Then `Super+Space` → "Workspace settings" → *Permissions* tab: grants Xaver *Editor* on the
workspace, and `agent:scribe` *write* on `app:dev.qos.word-processor` only
(`workspace_settings.rs:64-92, 1734, 1832`).

**Actions (cut to Xaver).** `Super+Space` → "Connect to peer workspace" → pastes the token →
Enter. The card reports the joined room; the workspace name flips to Quinten's
(`overlay_host.rs:1447, 1478`; `invite.rs:284-299`). Back on Quinten: Xaver's avatar appears in
the top-bar stack (P1 build — roster is seeded, not peer-fed, today: `state.rs:3871`).

**Said.** "That token is signed by Quinten's device key; it carries the room id and an expiry,
and the peer's identity is checked against it. Permissions are capabilities: a verb — read,
comment, write, invite, admin — on a scope, and the scope can be the whole workspace, one app,
or one action of one app. Xaver gets the workspace. The agent gets exactly one app. Every
decision is fail-closed and only the owner can change it (`state.rs:1018, 1080`)."

### Beat 3 — Installing the word processor (1:25–2:05) · Quinten

**Actions.** `Super+Space` → types `word` → row "Word Processor — not installed · Install &
open" (P0 build) → Enter. Status bubble: "Installing dev.qos.word-processor…" → window opens in
the strip beside Files.

**Said.** "The word processor isn't on this machine. A qpkg is a signed tar: manifest, a signed
root, blobs named by BLAKE3 hash. The registry entry is ed25519-signed, the root hash is
checked, then the package signature is checked again at install — fail-closed
(`docs/registry.md:36-72`). It runs in a Bubblewrap + Landlock sandbox with networking off,
because its manifest says so (`apps/word-processor/qos-app.toml`). So 'installing' is just
caching a verified artifact; nothing touched the system." *(Do not claim eviction on camera —
the GC exists only in an undeployed daemon, `crates/packaging/app-host/src/gc.rs`.)*

### Beat 4 — Writing together (2:05–3:05) · Quinten, then Xaver

**Actions (Quinten).** In Files, double-clicks `brief.docx` → it opens in the word processor
(`shell/src/shell/doc_open.rs`; the launcher mints a read grant for that one path,
`providers/files.rs:913-933`). Types a heading (`Ctrl+1`) and three bullet lines.

**Actions (cut to Xaver).** Opens Files → `Demo › brief.docx` → word processor opens with
Quinten's text already in it. Quinten's caret with a coloured name pill; new words appear as
he types (P0 builds a+b). Xaver clicks into the doc and types a line; cut to Quinten: Xaver's
caret and selection in his colour (`widget.rs:5646-5654`, `ui/src/presence/render.rs:33, 48, 143`).

**Said.** "Nothing is streamed. Xaver's machine runs its own copy of the app and draws it
locally; the two copies share one Automerge document. Each keystroke is a CRDT change that the
compositor's broker publishes over libp2p to the other session — the app itself never touches
the network; its sandbox doesn't let it. Cursors, carets, selections and name tags come from the
toolkit's presence layer, not from the app — every app gets them for free."

### Beat 5 — The image (3:05–3:35) · Quinten, then Xaver

**Actions (Quinten).** Drags `photo.png` out of Files and drops it on the document; it lands at
the drop point. (Fallback if the layout fights you on the day: toolbar *Image* button →
the OS file chooser, `shell/src/portal/file_chooser.rs`.) Optionally drags the corner to resize
(`word_processor.rs:630`). Mind the layout: a group laid out at full width (the first-boot
demo group is) slides its neighbour off screen when focused — keep Files at a normal width.

**Actions (cut to Xaver).** The image appears in his document.

**Said.** "The app can't read that folder. What it received is the file's bytes, handed over
by the OS as part of the drop — the app never sees a path it has no right to. The image is
bytes inside the same document, so it replicates like the text did."

### Beat 6 — The agent (3:35–4:35) · Quinten, both screens

**Actions (Quinten).** Types a last line: `Scribe: turn the bullets into a table`, then opens
`Super+Space` → "Room activity" or the word processor's *History* strip to show authors so far. (Off camera, already running: `qos-agent --workspace <room> --record file:/var/workspace/Demo/brief.docx` — it joined the room the way Xaver's app did.)

**Actions (both screens).** A third colour appears: Scribe's caret moves through the bullets,
a table replaces them, a caption lands under the image. Both windows update at once. The
history strip shows "Version 6 · Scribe · inserted table" (`history_panel.rs:532-536`).

**Actions (Quinten, optional — not real yet).** Workspace settings → Permissions → *Revoke* on the
agent's grant. The broker does not enforce per-principal grants on document traffic today, so this beat needs the broker gate (P0 follow-up) before it can be filmed honestly.

**Said.** "The agent has no window and no pointer. It joined the document the same way
Xaver's app did, and it edits with the same actions the toolbar dispatches — `SpliceText`,
`InsertTable` — which the app exports. We've proved those are byte-identical to what a click
produces (`ui/src/app.rs:7000`). So it works in parallel with both of us, in workspaces
nobody is looking at, and every change is attributed and revertible. It has the permissions
we gave it: write on this app. Revoke, and it stops."

### Beat 7 — Close (4:35–4:55) · Quinten

**Actions.** Room activity pane (`shell/src/shell/room_activity.rs`): invite, grants, rename,
revoke, each with an author and a *Revert* where allowed. Then the document history strip.

**Said.** "Everything that happened in the last five minutes is a change with an author — the
room's governance, and the document's contents. Collaborating with people and with agents is
the same problem: see what they do, see what they did, control what they can do."

---

## 2b. What landed from the parallel integration branch (merged 2026-08-22)

`origin/main` moved 76 commits while this plan was being executed. Relevant to the beats:
`scripts/yc-demo.py` (a three-session driver for the whole demo), `docs/agent-action-api.md`
+ `scripts/qos-mcp-server.py` + `scripts/headless-examples/mcp-live-proof.py` (a model driving
the compositor's action surface over MCP — a second agent mechanism beside `qos-agent`),
`shell/src/shell/peer_presence.rs` (app pointer presence relayed between a session and its
peers — part of P0-b), the workspace wheel body + `Super+S`, and `scripts/demo-home.py` /
`scripts/demo-record.sh` (a filming mode). Read those before re-planning beats 2, 4 and 6.

## 3. Build before shoot (ranked)

**P0 — the demo does not work without these**

| # | Work | Where | Size |
|---|---|---|---|
| a | Outbound CRDT bridge for the capnp broker: on `sendUnsignedIncremental` / signed path, also `net.publish_app_crdt_incremental(app_id, cluster_id, record_id, data)` like the JSON `reader_loop` does | `shell/src/shell/collab_broker_capnp.rs:601-603, 702-707, spawn at :1168` ← mirror `collab_broker.rs:1017-1031`; thread `peer_sync` through `native.rs:1416` | small, well-scoped; add a two-session test to `two-session-collab-demo.py` (type in app A, read in app B) |
| b | App-level presence across machines: publish `CollabRequest::Presence` as a `NeighbourMsg` on gossipsub and fan inbound to the route; attribute by user id, not `peer-{n}` | `collab_broker.rs:772-787`, `shell/src/network/protocol.rs:49` (compositor-level `PresenceMoved` already exists as a template), `collab_broker_capnp.rs` | medium |
| c | **DONE 2026-08-22 — landed from the parallel integration branch.** `crates/packaging/app-catalogue` + `shell/src/shell/catalogue.rs`: the palette offers an app the machine doesn't have in the *same* "Open X" row as an installed one; Enter fetches, verifies and opens. Proof: `scripts/install-on-open-demo.py`, `app-catalogue/tests/registry_to_installed.rs`. (A second, equivalent implementation built in this session was dropped at merge time.) Set-day rig: `scripts/demo-registry.sh up` still serves a signed local registry for it | `shell/src/shell/catalogue.rs`, `crates/packaging/app-catalogue`, `scripts/install-on-open-demo.py` | done |
| d | **DONE 2026-08-22.** `apps/agent` (binary `qos-agent`): joins the room + record through the session's JSON broker, gets the document from the word processor via the `Arrived` handshake, and carries out an instruction written in the document (`Scribe: turn the bullets into a table` / `caption` / `heading`) with the word processor's own exported actions, under its own actor — so the history strip names it and its caret pill reads "Scribe" (new optional `handle` on `Hello`). Proof: `apps/agent/tests/relay_roundtrip.rs` (cross-socket) and `scripts/headless-examples/agent-demo.py` (real compositor + real word processor, file read back). Scripted vocabulary; a model call would slot into `plan.rs`. Not done: the broker does not enforce the agent's grant, so the "revoke → refused" beat is not real yet | `apps/agent/`, `apps/word-processor/src/actions.rs` (`InsertParagraph`), `ui/src/collaboration.rs` (`Hello.handle`), `shell/src/shell/collab_broker.rs` | done |
| e | Two-machine LAN rehearsal of `/invite` → `/connect` → grant → revoke | `scripts/two-host-sync-test.sh` flow | a day, unknowns |

**P1 — polish that the camera will notice**

| # | Work | Where |
|---|---|---|
| f | *Copy* on the invite card; `Ctrl+V` in the connect prompt | `overlay_host.rs:9004-9160`, `:1514-1523` (`ui::clipboard::WaylandClipboard`) |
| g | Remote caret pill shows the display name, not the raw id | `apps/word-processor/src/widget.rs:6719` → `presence.display_name(user)` |
| h | Typed text tinted in the author's colour (notes has it, word processor doesn't) | add `draw_peer_typing` / `draw_peer_authorship` next to `widget.rs:5646-5654` (`ui/src/presence/render.rs:63, 192`) |
| i | Permissions composer can pick an `agent:` subject; agent rows labelled as agents, not "Other" | `workspace_settings.rs:889, 1686` |
| j | Top-bar avatar stack fed from live peers; join/leave toast | `state.rs:537, 3871`; `peer_join_toast.rs` (no caller) |
| k | Desktop-level remote cursors in native (only if you want cursors over Files too) | mount `RemoteCursorOverlay` (`remote_cursors.rs:44`) and pump like `hosted.rs:296-347` |

**Keep off camera** (exists but is fake or unwired): the `quickdesktop://invite` link + Copy in
Workspace Settings; QR pairing (`identity_flow` is only in an example); "nearby peers" (mDNS has
no list UI); any "window appears on the other machine" beat; legacy-app streaming (pipeline
complete, zero production callers: `compositor/state.rs:714`); the dead `GlobalSearch` wrapper
(`shell/src/shell/global_search.rs`); `Ctrl+W` / `Ctrl+T` / `Super+1..9` (not bound — close and
grouping are mouse-only).

---

## 4. Email: claims the code contradicts

Fix these before sending — Grey will watch the demo against the text.

| Draft says | Code | Suggested wording |
|---|---|---|
| "you get a recovery code" | Onboarding says the opposite and a test locks the copy: *"There is no recovery. qOS has no backup code, no recovery phrase…"* (`shell/src/onboarding.rs:63-70, 318-324`). Threshold/paper-share recovery exists as a library, unwired. | "An Ed25519 identity minted on first boot; share-based recovery is built as a library and not yet wired." |
| "compile for the exact instruction set the user's processor uses" | `.cargo/config.toml` pins `target-cpu=x86-64-v3` and rejects `native` by name. | Drop it. "Fat LTO, one codegen unit, `panic=abort`, stripped." |
| "Most default apps come in under 2MB" / "the ~2MB are downloaded and opened automatically" | **Now measured on both sides of the change, not estimated.** As shipped today the word processor (`--profile demo`, i.e. the release configuration) is **24,802,000 B / 23.65 MiB**, `.qpkg` **24,813,056 B**; `.text` alone is 19,167,280 B and `ldd` lists only `libc`/`libm`/`libgcc_s`, so the toolkit is inside the binary and there is nothing beside it to exclude. `ui` is now re-exported from `runtime-dylib`, and the app carries an opt-in `thin` feature; built `--profile thin-app --features thin` with `-C prefer-dynamic` the same app is **4,357,040 B / 4.16 MiB**, `.qpkg` **4,367,872 B** -- 82.4% smaller, `.text` down to 2,527,879 B (-86.8%), and `ldd` resolves `libruntime_dylib.so` + `libstd-*.so`. So "excluding the shared UI toolkit it links against" is now literally true of the binary -- but the figure is **4.16 MB, and 2 MB is still not met**. The toolkit is a **78,392,032 B / 74.8 MiB** shared object that ships once: larger than one static app, because a dylib must export everything and `thin-app` has to disable LTO, so no cross-crate dead-code elimination runs on it. Break-even is ~4 apps; across the 45 packaged apps it is 1.04 GiB of binaries today against 0.26 GiB shared. Caveats worth stating before anyone repeats the number: only the word processor has been converted and measured, the other 45 `ui` apps are a two-line change each but unbuilt; and neither the thin nor the *static* binary completed a headless frame on this machine tonight (both were killed mid-render under load, the static one at 282 s), so the thin app is proven to load and resolve its shared libraries, not yet proven to paint. The single largest identified item left in the 4.16 MB is `spellcheck`'s `include_str!`-embedded 707,859 B word list. | "Our apps are about 24 MB today. Moving the UI toolkit into a shared library takes the word processor to 4.2 MB -- measured on a real binary, not projected -- with the toolkit itself a ~75 MB OS component that ships once for every app. We are not at 2 MB, and one app has been converted so far." |
| "evicted under storage pressure, staying on the workspace host" | No eviction in `fs/` (moka cap is in-memory entry count, `fs/src/store.rs:41-51`); sibling readahead is an empty stub (`fs/src/smart.rs:26-29`); the relay pins CRDT/metadata, not blobs (`qos-relay` has no `iroh-blobs`). | "Content is fetched on open from whichever peer has it; an always-on host pins the workspace state." |
| "so do open apps with their full state" (cross-device) | App state is deliberately local and non-syncing (`crates/system/app-state/src/lib.rs:22-39`); `crates/session` is greeter/installer/power. | Cut, or mark roadmap. |
| "a normal GUI application can run on the server while its UI renders on your device" | Explicitly out of scope for v1 (`docs/superpowers/specs/2026-07-04-sync-host-model-design.md:186-190`); `apps/remote` qOS mode is hard-coded demo devices. | "A spare machine becomes the always-on host: pins state, relays, store-and-forwards while devices are offline." (`crates/hosting/qos-relay`, 13 integration tests.) |
| "Every file carries a changelog with authors, visible in the file explorer" | Only `.md/.odt/.docx` take the CRDT path (`crates/doc/qdoc/src/codecs/mod.rs:58-73`; `.xlsx/.ods` are off the FUSE path); history UI exists in the word processor only; Files shows sync state, no history. | "Documents — Markdown, ODT, DOCX — are stored as a change graph, so history, structural diff and authorship work at the OS layer; today that history is surfaced in the word processor." |
| "diffs, history and undo work the same as inside an app" | History + structural diff yes (`qdoc/src/diff.rs`, `history.rs`); OS-level undo no — undo is per tab. | Drop "undo". |
| "The YouTube app registers a search provider … called automatically whenever you search" / "get into the app at that position" | App-registered providers are in-process only and the compositor grabs `Ctrl+K` before the client sees it (`client_input.rs:571`). The global palette has seven providers; only word-processor and pdf_viewer records are reachable globally (`providers/records.rs`). | "Global search covers apps, files, notes and the document records apps write; in-app search is scoped to the app's own resources and jumps to the hit." |
| "Search for one, press enter, and they open. No explicit install process." | Store app only (beat 3 builds the palette path). | True after P0-c; say "from search or the store". |
| "a WebKit based browser we will move to Chromium" | The browser embeds **Servo** (`apps/README.md`, `plan/product.md` engine table). | "a Servo-based browser; engine decision open (Gecko vs Chromium)". |
| "every participant's cursor is emulated into one pointer" | Better than claimed: each peer gets its own virtual seat (`shell/src/network/streaming.rs:66-105`), pointer + keyboard only. But the share path has no UI caller. | "video stream with per-peer input injection" — and don't demo it. |
| "Collaborators are confirmed against it" | True in the relay handshake (`qos-relay/src/auth.rs`); the FUSE layer doesn't verify certs; the broker attributes ops from `QUICK_APP_ID`, not a key (`MISSING.md:139`). | Keep, softened: "peers authenticate with it". |
| "standard library" of actions (CRUD, text editing, navigation, selection) | No shared action library; each app defines its own enum; navigation/selection are explicitly not actions (`docs/action-model-recipe.md:19-26`). 33/39 apps export actions + resources. | "Apps export resources and actions through one registry (33 of our apps do); the shared verbs are the next step." |
| "Around 40 default apps" | 45 app dirs, all with manifests; status per `plan/product.md` "Functional status" (slides: no save; chat: calls stubbed; mail: no engine). | Fine as is. |

Claims that hold as written: strips/no overlap; per-workspace sandbox with Landlock + portals;
qpkg signing and verification; Flatpaks in the store; docx/odt word processor; calculator 2D/3D;
per-app permissions; CRDT state sync drawn locally; history with authors (word processor).
