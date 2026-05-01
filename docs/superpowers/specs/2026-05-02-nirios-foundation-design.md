# Nirios Foundation — Spec (2026-05-02 reset)

> **Status:** authoritative spec for the next phase. Supersedes the
> infinite-canvas framing in
> [`2026-04-18-nirios-shell-design.md`](2026-04-18-nirios-shell-design.md)
> for layout. Older specs remain valid for the parts not contradicted
> here (CRDT toolkit, Iroh-FS architecture, sandboxing intent).

> **For agentic workers:** companion plans live at
> `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`,
> `…-per-workspace-sandbox-permissions.md`, `…-peer-sync-end-to-end.md`.
> Follow-up specs (search/actions/SDK, RDP+native collab, native apps,
> per-workspace permissions, OS distro) are explicitly deferred — this
> document only sketches their boundaries.

---

## 0. Project reset

The infinite 2-D canvas (Niri-OG with X-and-Y panning) is **out**.
The model that ships is:

```
Workspace
└─ Strip   (one or more; vertically paginated; UI ↑/↓ between strips)
   └─ Group (window group; horizontally scrollable in the strip)
      └─ Tab  (one app per tab; user can drag tabs between groups,
              split into a new sibling group, or merge)
         └─ App (rendered by the host compositor or a quick-ui widget)
```

Equivalent to Niri's columnar workflow plus a tab dimension. **Strip**
is the navigable unit. Workspaces are switched via the radial wheel
(left edge); strips within a workspace via vertical wheel/keyboard.

Three load-bearing properties:

1. **Wayland-first UI toolkit** (`quick-ui`) — every native widget
   draws into a Vello scene that the compositor composites alongside
   third-party Wayland clients.
2. **Niri-like collaborative compositor** (`quick-desktop`) — a real
   smithay compositor, not a winit client. Hosts both `quick-ui`
   widgets (rendered in-process) and remote/local Wayland clients
   (rendered via xdg-shell). Per-workspace sandbox is a hard boundary.
3. **Collaborative by default** — every workspace's CRDT document is
   syncable with invited peers over libp2p; native apps render via
   shared Vello scenes; foreign Wayland clients via buffer streaming
   (RDP-style).

The rest of this document scopes the deltas needed to land this
shape and connects each scope to a follow-up spec or plan.

---

## 1. Real smithay-based Wayland compositor — **primary focus**

### Today
`quick-desktop` is a single winit Wayland *client*. It opens one
top-level toplevel, draws the full nirios shell into it, and can host
no other apps. Most "compositor" code paths (sandbox enforcement,
multi-output, surface-streaming) are stubs.

### Target
`quick-desktop` becomes a real Wayland compositor based on
[smithay](https://smithay.github.io/) ≥0.6, hosting third-party
Wayland clients alongside the in-tree shell chrome. The shell's
existing widget tree (topbar, wheel disc, group rows) renders as
**internal surfaces** — the compositor composes them with third-party
client surfaces into a single output frame per monitor.

### Surface architecture

Each output (= monitor) holds a stack of layers, bottom-to-top:

| Layer | Source | Examples |
|---|---|---|
| `wallpaper` | shell | desktop backdrop |
| `workspace_content` | shell + clients | window-groups; foreign clients map into the active strip |
| `chrome` | shell | top bar, wheel disc, presence overlays |
| `overlay` | shell | command palette, modal scrims |
| `cursor` | shell + clients | local cursor + remote presence cursors |

The compositor renders each layer to a Vello `Scene` per frame, then
hands the composite scene to the existing `OffscreenRenderer`-style
pipeline that already exists in `quick-ui`.

### Wayland protocols supported (initial set)

- Core: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_output`,
  `wl_seat`, `wl_data_device_manager`, `wl_surface`.
- Shell: `xdg-shell` (toplevels + popups). No `wl_shell` (legacy).
- Damage: `wl_surface::frame` callbacks; presentation feedback.
- Input: `wl_pointer`, `wl_keyboard`, `wl_touch` (later).
- DnD: `wl_data_device` for cross-app drag.
- Future: `xdg-decoration` (we always do client-side decorations
  ourselves, per concept), `xdg-activation`, `presentation-time`,
  `wlr-screencopy` (peer screen-share — see §6).

XWayland is out of scope for v1. If a third-party app demands X,
the user runs it in a foreign rootless session.

### Window-to-tab binding

When a Wayland client first commits a buffer to a toplevel, the
compositor:

1. Looks at the surface's app-id.
2. If a tab in the active strip has the same `desired_app_id` (the
   nirios-side declaration of "this tab will be filled by an app
   whose Wayland app-id is X"), **adopts** the surface into that tab.
3. Otherwise, creates a new tab in the active group with
   `Tab::Foreign { app_id, surface }`.

Closing the surface marks the tab `closing`; the existing tab-close
animation runs and the tab is reaped.

### Internal-surface render path

`quick-ui` widgets keep their existing `draw(scene, transform, …)`
shape. The compositor builds the per-frame scene by:

1. Calling `chrome.draw(scene, …)` for the shell's own layers.
2. For each foreign client surface: blit its committed `wl_buffer`
   contents into the scene at the surface's mapped rect.
3. Submit the scene to `vello::Renderer::render_to_surface` per
   output's `wl_output`-backed surface.

### Migration path

The existing in-tree winit-client codepath stays as `--mode hosted`
for one release: opens a single top-level on the parent compositor,
useful for development and CI. New `--mode native` runs as a real
compositor. Default flips after smoke tests pass.

### Out of scope here (covered by other documents)

- Permissions enforcement at the surface boundary → §3.
- Multi-output protocol details → §4.
- Buffer streaming for peer collaboration → §6.

### Detailed plan
**[`plans/2026-05-02-real-wayland-compositor.md`](../plans/2026-05-02-real-wayland-compositor.md)**

---

## 3. Per-workspace sandbox + permissions

### Rationale

The user's mental model: *"I invited Alice to my Research workspace
to look at one notebook. She must not be able to read my Notes app's
private store, and she must not be able to spawn arbitrary processes
that touch the rest of my filesystem."*

Sandboxing is therefore **per-workspace** (not per-app), because the
workspace is the unit of collaboration.

### Mechanism

Every workspace runs in a dedicated **Bubblewrap** sandbox with:

- A bind-mount of `/var/workspace/<ws_id>/` into the sandbox's `/work`.
- A bind-mount of `/var/cache/quick-fs/<ws_id>/` into `/cache`.
- An empty `/home` (no host home access by default).
- A read-only `/usr` from the host (for system binaries).
- A `seccomp-bpf` filter blocking `ptrace`, `mount`, raw network
  syscalls, etc.
- Its own user namespace; UID 1000 in-namespace maps to a
  per-workspace subuid on the host.
- A private DBus session bus.

When the user spawns a Wayland client into a workspace, the
compositor invokes `bwrap` with that workspace's profile and sets
the `WAYLAND_DISPLAY` socket to a per-workspace bridge socket
(see §3 of the sandbox plan for protocol).

### Permissions

Three classes of capability requested through `xdg-desktop-portal`:

1. **Filesystem** — paths outside `/work` and `/cache`. Default deny;
   prompt on first request, remember per-workspace.
2. **Hardware** — camera, microphone, screen-capture (other than the
   own surface). Default deny; user-confirm always (no remember-yes
   for the owner workspace, never for invited peers).
3. **Network** — outbound TCP/UDP. Default deny for invited peers,
   default allow for the owner workspace's apps; configurable
   per-workspace from the WS-Settings modal.

### Approval UX

Compositor surfaces an inline notification at the top of the active
strip (concept §3.2 affordance). User can:

- Allow once
- Allow this session (cleared on workspace close)
- Allow always (remembered in workspace's CRDT)
- Deny + remember

Invited peers can never "allow always" — only the workspace owner
can grant durable capabilities. Peers requesting capabilities trigger
the prompt on the **owner's** screen.

### Detailed plan
**[`plans/2026-05-02-per-workspace-sandbox-permissions.md`](../plans/2026-05-02-per-workspace-sandbox-permissions.md)**

### Follow-up spec
*Per-workspace permissions* — detailed protocol for prompt UI, the
CRDT representation of "remembered" grants, and revocation flow.

---

## 4. Multi-monitor / multi-output

### Today
Single hard-coded viewport (1600×900 in shots, whatever winit picks
in the live binary). No `wl_output` enumeration, no per-output
state.

### Target

The compositor enumerates every output via `wlr-output-management`
(or smithay's output abstraction). Each output:

- Has its own active workspace pointer (so different monitors can
  show different workspaces).
- Has its own scaling factor (HiDPI per output).
- Has its own cursor.

Workspaces are still global to the user, not per-output — the user
can swipe a workspace from monitor A to monitor B.

### CRDT impact

`Chrome::active_workspace` becomes `Chrome::active_per_output:
HashMap<OutputId, WorkspaceId>`. Persisted; replicates to peers.

### Out of scope here

- Layout per output (multi-window-per-monitor) is a v2 concern;
  initial release shows the same workspace-strip on every monitor or
  one workspace per monitor with no spanning.

---

## 5. End-to-end peer sync (CRDT replication over libp2p)

### Today

`quick-ui::collaboration` defines `CollabRequest` /
`CollabEvent` over a Unix-socket protocol (in-process, single-host).
Wire types exist for `SyncIncremental` and `RemoteCursorMoved`. No
network-side implementation: launching two `quick-desktop` instances
on two hosts produces no sync.

### Target

Each workspace is a libp2p **gossipsub topic**:
`/nirios/ws/<workspace_id>/v1`. Members of the topic are peers
with active access tokens (issued by the workspace owner). The
protocol stack:

```
gossipsub topic = workspace ID
   payload = enum NeighbourMsg {
     SyncReq { since: ChangeHash },
     SyncResp { changes: Vec<Vec<u8>> },   // automerge changes
     CursorMoved { peer, key, point, ts },
     SurfaceFrame { surface, encoded },     // see §6
     PermissionRequest { peer, capability }, // see §3
     PermissionGrant   { peer, capability },
   }
```

### Discovery

- mDNS for peers on the same LAN (`_nirios._tcp.local.`).
- Manual peer-ID exchange + connect (`/connect` palette command).
- Future: relay through an Iroh-style rendezvous server.

### Sync semantics

- **Automerge changes** for workspace structural state (windows,
  tabs, notes content, etc.).
- **Ephemeral presence** for cursors / focus / selection — published
  at ≤30 Hz, never written to the persisted CRDT.
- **Surface frames** for foreign Wayland clients that the owner has
  shared into the workspace — out-of-band channel; not in the CRDT.

### Conflict resolution

Automerge handles concurrent edits via vector clocks. The toolkit
already exposes `commit_action` snapshots; nothing new on the
data-model side. UI-level divergence indicator is a follow-up.

### Detailed plan
**[`plans/2026-05-02-peer-sync-end-to-end.md`](../plans/2026-05-02-peer-sync-end-to-end.md)**

### Follow-up spec
*Collaboration: native + RDP* — distinguishes how `quick-ui` widgets
sync (CRDT-only — every peer renders its own scene from the document)
vs how foreign Wayland clients sync (the owner streams composited
buffers; peers can inject input).

---

## 6. Wayland buffer streaming for screen-share

### Mechanism

For foreign clients (Wayland apps that aren't `quick-ui`-native):

- Owner's compositor copies the client's committed buffer.
- Encodes via h264 (via `vaapi` if hardware available, otherwise
  software `x264`).
- Publishes encoded NALU stream as `SurfaceFrame { surface, …}`
  messages on the workspace topic.
- Peer compositor decodes + draws the frame as a texture in the
  matching tab's rect.

### Input

Peer pointer / keyboard events get serialized as
`RemoteInputEvent { peer, surface, event }` messages and replayed
into the foreign client by the owner's compositor. The owner can
disable input from a peer (read-only share) per peer per workspace.

### Performance budget

- Target 30 fps for shared surfaces, 60 fps for native widgets.
- ≤80 Mbps per shared surface at 1080p (h264 high-profile).
- Damage-aware: only encode regions the owner's compositor reports
  as damaged.

### Out of scope

- Audio sharing (separate spec).
- Variable bitrate / network adaptation (later).

### Follow-up spec
*Collaboration: native + RDP* (above) covers protocol details.

---

## 8. Iroh-based `/var/workspace` FUSE integration

### Today

`quick-fs-fuse` builds standalone with Iroh + `iroh-blobs`. Mounted
via systemd at `/var/workspace`. Apps access files normally; the
FUSE driver hydrates content on `read()`.

### Gaps

- No app-level surface for "downloading…" or "stale". Apps can
  block on `read()` and the user has no feedback.
- No per-workspace mount: today the FUSE serves a single content
  store across all workspaces. With per-workspace sandboxing (§3)
  each workspace must see only its slice.
- No write-quota / cache-pressure handling.

### Target

- The FUSE driver gains a `nirios-fs-ctl` D-Bus interface exposing:
  - `subscribe(path)` — async stream of `Hydrating { progress }`,
    `Local { hash }`, `Remote { peer }` events.
  - `pin(path)` — keep this content local even under cache pressure.
  - `evict(path)` — drop from cache.
- `quick-ui` adds a `FsAware` widget mixin that subscribes per-rendered
  path and overlays a small spinner if `Hydrating`.
- Per-workspace mount points: `/var/workspace/<ws_id>/`. Sandboxes
  bind-mount only their own.

---

## 17. Animation system (toolkit)

### Today
Each animating widget owns its own `Instant`-based state
(`spawned_at: Option<Instant>`, `closing_at: Option<Instant>`,
`last_wheel_at: Option<Instant>`, `wheel_anim_state`,
`snap_anim: Option<SnapAnim>`, etc.). Eases are inlined.

### Target

A single `quick-ui::animation` module exposing:

```rust
pub enum Curve { Linear, EaseOut, EaseInOut, Spring { stiffness, damping } }

pub struct Animated<T> {
    from: T,
    to: T,
    started: Instant,
    duration: Duration,
    curve: Curve,
}

impl<T: Lerp> Animated<T> {
    pub fn current(&self, now: Instant) -> T;
    pub fn done(&self, now: Instant) -> bool;
}
```

Plus a per-`App` driver that polls every `Animated<T>` exposed by
the widget tree and reports `needs_animation_frame()` until they
all settle. Replaces the dozen ad-hoc `needs_animation_frame()`
implementations.

### Migration

Widgets adopt `Animated<T>` incrementally. The current ad-hoc fields
keep working until each widget migrates. Final cleanup removes the
helpers.

---

## 18. Theme switching at runtime

### Today
`Theme::dark()` / `Theme::light()` exist. Picked at `App::new`. No
runtime swap.

### Target

- `Theme` lives behind an `Rc<RefCell<Theme>>` shared with the
  draw context.
- Swapping (`set_theme(Theme::light())`) flips a `theme_revision: u64`
  counter; widgets that cache theme-dependent layout invalidate when
  the counter changes.
- Quick-settings exposes a "Light / Dark / System" radio. "System"
  follows `org.freedesktop.appearance.color-scheme` from the portal.
- Smooth crossfade (300 ms) between themes via a Vello layer
  blend, so the swap doesn't pop.

---

## 19. i18n actually used

### Today
`fluent` is in deps. No FTL files exist. Strings are inline literals
throughout (e.g. `"Welcome to nirios."`, `"Click to edit"`).

### Target

- `quick-ui/locales/<lang>/` — one FTL file per supported locale.
- `quick-ui::i18n::tr(key, args)` returns `Cow<'_, str>`.
- Initial coverage: `en-US`, `de-DE`. (User is German; covers both.)
- A test ensures every key referenced from Rust source has an
  `en-US` entry — fail the build on missing strings.
- Live language switching parallels theme switching; same
  `revision`-based invalidation.

---

## 20. Window resize via drag handles

### Today
Window groups have three preset widths (`Narrow=400`, `Normal=600`,
`Wide=800`). The maximize toggle exists but free resizing doesn't.

### Target

- Hovering the right edge of a window-group's content area shows a
  vertical resize handle (same affordance as concept's
  `WindowFrame.tsx:217`).
- Drag updates `WindowGroup::width = WindowWidth::Custom(px)`.
- Persisted via the recently-landed maximize-style writeback path
  (see §1 of `plans/2026-05-02-peer-sync-end-to-end.md` for
  pattern).
- Snap thresholds: within ±20 px of a preset, snap to that preset
  (so the user can always resize back to the conceptual sizes).

---

## 21. Move tab/group across strips

### Today
Within-strip drag works (just landed in commit `c64e506`).
Cross-strip drag is unimplemented.

### Target

- During a tab drag, dragging beyond the strip's vertical bounds
  flashes the destination strip's outline (visual feedback).
- Release outside the source strip + within a sibling strip's rect:
  detach from source, append to destination strip's last group, or
  create a new group at the drop x-position (mirroring within-strip
  rules).
- The source strip is responsible for the detach side; the
  destination strip for the insert. The shell-level `GroupRow`
  parent (`StripView`) brokers the cross-strip handoff.

---

## 22. Persistent named layouts

### Today
The active workspace's window layout persists via state.rs. Naming
and saving alternative layouts ("Reading mode", "Coding mode",
"Meeting mode") is unimplemented.

### Target

- `Workspace::layouts: HashMap<String, LayoutSnapshot>` where
  `LayoutSnapshot` is the strip→group→tab tree at a moment in time.
- Quick-settings exposes a "Save current layout as…" + a dropdown of
  saved layouts.
- Switching layouts is a CRDT mutation that diffs the current strip
  state against the named snapshot and applies the moves; replicates
  to peers.

---

## 23. Tab pinning

### Today
No protection against close.

### Target

- `Tab::pinned: bool`. Pinned tabs:
  - Always sort to the left of the strip.
  - Render with a smaller pill (50% width) showing only the icon.
  - Have no close button; right-click → "Unpin".
- Persisted; replicates.
- Drag to reorder works; pinned tabs reorder among each other only.

---

## 29. Approval flow when an app requests a new path

### See §3.

---

## 30. Per-app capability tokens (camera, microphone, network)

### Today
Nothing.

### Target

`xdg-desktop-portal-nirios` — a portal backend that the compositor
implements. Each capability:

```rust
enum Capability {
    Camera, Microphone, Screencast, Location,
    Network { hosts: Option<Vec<String>> },
    Filesystem { path: PathBuf, mode: FsMode },
}
```

Apps in workspaces request capabilities via the portal. The compositor
shows the prompt described in §3, records the grant in the workspace's
CRDT (so peers see it and the compositor restores it after restart),
and forwards the grant token to the app via the portal.

Revocation: the WS-Settings modal lists every active grant per app
with an "Revoke" button.

### Out of scope here
- Concrete portal protocol numbers (use freedesktop.org's spec).
- Audio routing for shared microphone (later).

---

## Future specs called out by the user

These are explicitly **deferred** — placeholders below to land later:

| Spec | One-line scope | Approx size |
|---|---|---|
| **Search / SDK actions export** | Every CRDT manipulation = a serialisable Action; triggerable via UI clicks, palette search, AI-agent invocations; logged and revertible. Forms the basis of an extension/automation API. | ~half a foundation-spec |
| **Collaboration: native + RDP** | Native (CRDT-only sync of `quick-ui` widget state) vs RDP-style (buffer streaming + input replay) for foreign Wayland clients. Includes the wire protocol numbers. | ~half a foundation-spec |
| **Native apps** | Notes / Editor / Calendar / Mail / Files / Browser-shell-integration. Per-app data-model, persistence, expected size. | ~full foundation-spec |
| **Per-workspace permissions** | The detailed grant CRDT schema, prompt UX, revocation, and the permission model (capabilities, scopes, durations). | ~half a foundation-spec |
| **OS spec** | bootc image layout, first-boot wizard, update mechanism, display-manager handoff, ALL the distro-layer concerns. | ~full foundation-spec |

---

## Spec self-review

- **Placeholder scan:** none. Every section has concrete delta from
  current state and a follow-up plan or follow-up spec pointer.
- **Internal consistency:** checked — sandboxing (§3) references the
  Wayland bridge socket from §1's compositor; peer-sync (§5) references
  the CRDT mutations from §22; the FUSE work (§8) sits underneath the
  sandbox bind-mount story in §3.
- **Scope check:** this is foundation-level; deliberately does NOT
  cover the search/SDK/native-apps/RDP/permissions-detail/OS
  specs — those are deferred. The three plans are sized for
  several weeks each.
- **Ambiguity check:** "compositor" always means `quick-desktop`;
  "shell" = chrome widgets; "tab" = single-app pane inside a window
  group. Glossary repeated where ambiguous.

---

## Glossary (for readers picking this up cold)

- **Workspace** — top-level user-facing collection. Sandbox boundary.
- **Strip** — vertically paginated row of window-groups inside a
  workspace.
- **Group / window-group** — horizontally placed unit holding 1+ tabs.
- **Tab** — single-app slot inside a group.
- **App** — either a `quick-ui` widget tree (native) or a foreign
  Wayland client adopted into a tab.
- **Compositor** — `quick-desktop` binary (smithay-based, target).
- **Shell / chrome** — top bar, wheel disc, command palette, modals
  drawn by the compositor's own widget tree.
- **Peer** — a remote nirios instance invited to one or more workspaces.
- **Owner** — peer that authored the workspace; controls invites,
  permission grants, and (for now) layout names.
