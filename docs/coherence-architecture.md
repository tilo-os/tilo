# qOS coherence architecture — how the layers become one system

The through-line, top to bottom, every layer keyed on the same two vocabularies —
**one identity** and **one typed action/resource model**:

```
identity (TPM+FIDO2)                     ← qos-user-identity
  └─ workspace = {host, collaborators, permissions, virtual desktops}
       └─ state = CRDT docs + a fuse-fs subtree           ← automerge + fs (iroh/blake3)
            └─ actions  (typed, signed, replayable, permission-gated, EXPORTABLE)
                 ├─ presentation  = pure local render of the state the actions produced
                 ├─ presence      = who/where/doing, from the collab transport
                 └─ search (Ctrl+K) = query over the resources actions operate on
```

**Actions are the universal currency.** Collaboration *replicates* them, permissions
*gate* them, search *finds their resources*, an LLM *calls* them, version history *logs*
them, and presentation is a *pure function* of the state they mutate. Get the action
model right and the rest composes.

---

## 1. Hosting / sync — "workspace has one host; every device renders locally"

There is **no single cloud**. The unit of sync is a **workspace**: its CRDT doc-set +
its fuse-fs subtree, replicated between an authoritative **host** and its **clients** over
iroh. A host is any qOS machine in host mode: your laptop (P2P invites), your always-on
home server, or a company cluster (`qos-relay`, `compositor/hosted.rs`).

- **Phone + desktop + home server sync:** all three are *clients* of one personal
  workspace whose durable host is the home server; when the server is off they P2P and
  it catches up on return (per-workspace **durability policy**: designated durable peer).
- **Per-app backend placement (new affordance):** an app's CRDT doc has a *home*
  (host identity + relay addr) that is **re-homable** — "sync this Notes with `<server>`"
  decouples per-app backend from the workspace. Not one cloud; placement per app.
- **Managing your server = switching to its workspace.** Rendering is client-side, so
  self-host management is a normal hosted-workspace session against the server's **admin
  workspace** (users, roles, installed apps, firewall, storage, which app-backends run
  here) — rendered on your device, executed against the server. The only "special app"
  is that admin console.

## 2. Collaboration + identity + permissions

- **Identity is the root** (one crypto identity across a person's devices; disk unlock,
  homed login, op-signing, P2P device id — all the same key).
- **Workspace = team = permission boundary = host.** Different workspaces = different
  teams. Collaborators are identities granted in with roles.
- **Signed-op collaboration.** CollabRequest/CollabEvent carry signed CRDT ops
  (collab-types validator). The **host validates each incoming op against the actor's
  role + per-app caps before applying/relaying** — permission enforcement is op-time.
- **Ad-hoc / limited invites = attenuable capability tokens.** A signed invite grants
  {this identity, this workspace, this role, optionally time-boxed, optionally one
  sub-resource}. Tokens can only *narrow*, never widen.
- **Off-qOS reach.** (a) *Our apps off qOS*: they're Rust + the ui toolkit + the collab
  library — run as a normal binary on any OS, speak iroh+CRDT to a workspace host; OS
  integration (portals/fuse/presence) degrades gracefully. The collab layer is a
  **library, not an OS feature.** (b) *Generic non-qOS Linux apps*: can't speak CRDT →
  compositor surface-stream + input-injection (shared-screen-with-multi-input). Kept as
  the honest fallback tier; not true co-edit.

### Permissions — capabilities, not Unix mode bits
A permission is `(subject: role|user) × (scope: app | resource-type | resource-instance
| fs-path) × (verb: an exported action, or a coarse cap read/write/invite/admin)`. Roles
are named cap-bundles; users get roles + individual overrides + per-resource shares
("share this doc with Bob, view-only"). Because **actions are typed** (§3), a permission
like "member may delete comments" is literally `(role:member, resource-type:comment,
verb:delete)` checked at op-application.

**FUSE reconciliation:** the FUSE driver is the **enforcement point**. Every open/read/write
is checked against the workspace ACL keyed on *identity*, not archaic mode bits. Unix mode
bits are a **synthesized compatibility veneer** (sandboxed apps see a consistent view; the
app runs as the user, the *fs* enforces per-identity policy). For CRDT-understood files,
permission can be **per-action** (Bob may comment, not edit) because the host validates the
CRDT op, not just the byte-write.

UX (make-or-break): a **role × capability-group matrix** + a per-user override list + a
per-resource "shared with" panel. Complexity is tamed by grouping caps and defaulting to
sane roles (owner/editor/commenter/viewer) that a user can then refine.

## 3. Action / presentation / LLM / search

- **Search UX decision: Ctrl+K overlay is canonical; no per-app search implementations.**
  The palette queries the *focused app's exported resources* (scoped) or *all providers*
  (global). Apps that want a visible field use a thin `SearchField` that opens the *same*
  palette scoped to them — one implementation, zero duplication. Search does **not live in
  the app**; the app only *exports resources + actions*.
- **Off-qOS:** the palette ships with the toolkit (app-scoped works anywhere); only the
  *global* provider (fs full-text, all installed resource types) is qOS-only. Graceful.
- **LLM-navigable = actions exportable headlessly.** Resources are typed, addressable
  (`ResourceId`), nested, and carry actions. An external caller (LLM/script/app) can
  enumerate → select by id → invoke an action **without rendering** — this is exactly why
  action ≠ presentation.
- **Security of CRDT-native understanding:** format parsing (untrusted office/markdown →
  CRDT ops) runs **sandboxed + bounded** (XXE/zip-bomb/entity limits); op-application is
  signed + permission-checked. Understanding lives in the sandbox, not the trusted base.
- **Result security + ranking:** providers are **permission-aware** — filter at source
  (you only get resources you hold a cap to see). Ranking = `tier (ProviderTier) × match
  score × recency`, boosted for this-instance scope; the palette does the final
  cross-provider merge/rank.

---

## Build status vs this design (2026-07-09)
- **In flight (agents):** action model + Ctrl+K wiring · presence layer · CRDT-native
  odt/docx · desktop-env (resize/reorder/h-scroll/drag-file) · fractional scaling · HDR ·
  terminal redesign.
- **Next coherence wave (after these land):** (1) workspace/host/sync runtime +
  per-app-backend placement + durability (on qos-relay + hosted.rs); (2) capability
  permission system + FUSE enforcement + the role-matrix UI; (3) make Ctrl+K canonical
  (retire app search bars, permission-aware ranked providers, headless action API);
  (4) the admin-workspace app for server management.
