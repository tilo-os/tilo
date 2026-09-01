# qOS OS-integration plan — the 6 phases from apps to a coherent OS

The app layer + UI toolkit are far ahead of the OS spine. This plan closes the gap
described in `coherence-architecture.md`. **Methodology for every phase: assess → test →
integrate → prove.** Most of this work already exists (on `main` or on unmerged
concurrent-session branches); the job is to verify it actually works, integrate it, and
build only genuine gaps. Never trust a claim (including this doc) without running it.

## The decided foundation
**Sync/host model = one personal workspace; devices are clients/renderers of it.** A
"host" is an always-on device that pins the workspace CRDT and relays / store-forwards.
SyncThing-mode = a workspace with no dedicated host (peers sync directly). Home-server /
enterprise = the same workspace with its host being that machine. Everything downstream
(permissions, per-app backend placement, admin console) keys off this.

The executable device-role, unified replication-intent and deterministic
multi-server placement foundation is documented in
[`device-mesh.md`](device-mesh.md). It deliberately reuses collaboration ops,
filesystem blobs and consented remote input; app serving/compute execution is a
later worker implementation, not a new sync or identity stack.

## Phases (in build order; each = assess+test+integrate+prove the mapped work)

1. **Workspace = unit-of-sync runtime (host + clients + durability).**
   - On `main`: `crates/hosting/qos-relay` `WorkspaceRuntime` (pinned CRDT, membership,
     author-excluded fan-out, disk store-and-forward), tested.
   - Branch: `qos-relay-federation` (⊃ `qos-relay-daemon`) — deployable host (Containerfile.host,
     systemd, firstboot) + shell wiring (`shell/src/network/identity.rs`).
   - Prove: two member devices + host; delta A→B via fan-out; B offline → store-forward → converge.

2. **Capability permissions + FUSE enforcement.**
   - On `main`: `collab-types` `Role`/`OpClass`/`role_permits()`.
   - Branch: `sp1-user-identity` (+4). Identity root = `crates/identity/qos-user-identity` (TPM+FIDO2).
   - Build gap: `(role|user) × (scope: app|resource-type|instance|fs-path) × verb` enforced at
     op-application AND in the FUSE driver (per-identity ACL, mode-bits as a synthesized veneer);
     attenuable invite tokens. Prove: a role that may comment-not-edit is enforced on the real op path.

3. **Action model + search made canonical.**
   - On `main`: `collab-types` op/validator; a `command_palette` widget.
   - Branches: `search-providers-protocol` (+6), `shell-global-search-registry` (+8).
   - Build gap: typed exportable/replayable actions across apps; Ctrl+K canonical (retire per-app
     search bars); permission-aware ranked providers; a headless action API (LLM-navigable).
     Prove: enumerate → select-by-id → invoke an app action WITHOUT rendering.

4. **Thin apps (shared runtime) + qpkg wired into the store.**
   - On `main`: registry spine (`app-registry`), sandbox (`launcher`), `runtime-bundle` crate.
   - Branches: `shell-app-model-integration` (+6), `registry-infra` (+3), `apps-store` (+5),
     `apps-qpkg-fleet` (+10), `ci-app-model` (+8).
   - Build gap: apps link the shared runtime dylib; store
     Open/add-not-install path; a deployed registry. Prove: one relinked app <2MB runs.
   - Measured 2026-08-23 on the word processor, the first `ui` app relinked this way
     (`ui` re-exported from `runtime-dylib`, app built `--profile thin-app --features thin`
     with `-C prefer-dynamic`): 24,802,000 B → 4,357,040 B, `.text` 19,167,280 B →
     2,527,879 B, `.qpkg` 24,813,056 B → 4,367,872 B. The shared object is 78,392,032 B and
     ships once — a dylib exports everything and `thin-app` must disable LTO, so it gets no
     cross-crate dead-code elimination; break-even is ~4 apps, and 45 packaged apps go from
     1.04 GiB to 0.26 GiB. So the <2MB target is NOT met at 4.16 MB, and the remaining
     binary is app code plus the crates outside the toolkit (`qdoc*`, `spellcheck` — whose
     707,859 B word list is `include_str!`-embedded, the largest single item found).

5. **Peer-app local render (the collaboration keystone).**
   - `legacy-collab-*` = the OLD surface-stream+input-injection approach (the honest fallback tier;
     superseded for our apps). Reference `frame-link-demo`, `flatpak-collab-viewer` (merged).
   - Build gap: our apps (ui + collab lib) render a peer's session locally via CRDT co-edit, not
     H.264 streaming. Prove: two peers co-edit a doc, both render locally, cursors/presence live.

6. **Desktop-env polish + daily-driver gaps.**
   - Branches: `hdr-foundation` (+2), `flatpak-peerview-hdr` (+7), `flatpak-color-management-v1` (+5),
     `flatpak-hevc-main10` (+8), `presence-ui-polish` (+4), `systemd-boot-swap` (+4).
   - Build gap: horizontal window resize, better reorder, real h-strip scroll, drag-file-to-open;
     fractional scaling; finish HDR; notifications, session-lock, power-mgmt, audio routing, AT-SPI a11y.

## Stale/divergent branches (reference only — off an old base, not clean merges)
`docs/sync-host-model`, big `app-model-*`, `legacy-collab-*`, `nirios-shell`, `apps-fleet`,
`worktree-*`, `feat-qos-tts`, `scrub-planning-refs`. Local unpushed `wt-{actionmodel,presence,
deskenv,fracscale,hdr}` are likely superseded by the concurrent-session branches above.

## Phase 6c — daily-driver subsystems (mostly already built; corrected 2026-07-11)
A careful re-grep overturned the first-pass "settings-UI-only" read — most of these ARE on `main`;
the pattern from phases 1–5 (≈80% built) holds even here. Status:

- **Notifications** — ✅ **on main + extended.** A real `org.freedesktop.Notifications` zbus service
  (`shell/src/notifications.rs`) + shell-owned model + themed toast overlay, D-Bus round-trip tested.
  Added **history + Do-Not-Disturb** (`5e4b6e7d`).
- **Accessibility (AT-SPI)** — ✅ **on main.** `ui/src/accesskit_bridge.rs` + `screen_reader.rs` +
  `a11y.rs`, wired into button/link/toggle/tree_view. (Depth of coverage per-widget is the follow-up.)
- **Session-lock** — 🟡 idle/session scaffolding exists (`shell/src/compositor/idle.rs`); confirm/finish
  `ext_session_lock_v1` + a lock surface reusing the greeter's auth.
- **Audio routing** — 🟡 PipeWire infra (screencast) + media engine + a `wpctl` settings pane exist;
  per-app sink/source routing + device hot-switch UI is the genuine remaining build.
- **Power management** — 🔴 thinnest: settings pane (powerprofilesctl) only; deeper logind/UPower
  (suspend-on-idle, lid-switch, battery thresholds, inhibitors) is a real build.

Genuine remaining daily-driver work: **power-management (logind/UPore)** and **per-app audio routing**;
session-lock likely a small finish. Notifications + AT-SPI are done. Don't fake completion of the rest —
this is the tracked tail.
