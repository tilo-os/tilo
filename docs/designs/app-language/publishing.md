# Publishing tilolang apps

A tilolang app is a small, platform-independent bundle: compiled WebAssembly, metadata,
`app.ids`, and translations, typically tens of kilobytes to ~1–2 MB. Because every view draws
through the shared app window and every action executes inside the sandboxed document service,
there is no native toolchain, no per-architecture build matrix, and no ambient process
authority.

That lets the whole lifecycle happen inside Tilo:

1. **Develop and iterate in Chat (or an editor)** — a generative view or prototype evolves
   directly into a named package with live reload and reversible data.
2. **Publish in one step to two scopes** — straight into the current workspace with no external
   account, or to the public registry from inside Tilo.
3. **Open with zero install** — search, click a link, or open a shared resource and the package
   fetches, verifies, compiles, and opens on demand inside a strict capability boundary.

`docs/publishing.md` remains the guide for native/Rust apps, connectors, and CI builds. This
document covers the tilolang experience end to end.

---

## 1. Develop and iterate in Chat or an editor

People and agents already write live tilolang views in Chat: an agent answers with a
`WrittenView` kept in `apps/tl/views` (`agent-ux.md` §3, `crates/lang/tilolang/src/written.rs`).
Today a `WrittenView` holds only `view` and `fn` declarations against existing apps' types.
Growing that prototype into a real app should require neither leaving Tilo nor dropping into a
CLI checkout.

### 1.1 The draft package resource

A work-in-progress app is a workspace resource — a `DraftPackage` — edited through typed
actions like any other document:

- **Source files**: `app.tl` and any additional `.tl` modules, plus `locales/*.ftl`.
- **`app.ids` lock**: the 8-character base32 declaration IDs (`crates/lang/tilolang/src/ids.rs`),
  maintained automatically by the compiler and rename actions.
- **Metadata**: optional Store presentation (`name`, `tagline`, `description`,
  `supported_languages`, `images`, `source_url`, `homepage`).
- **Declared scenarios**: named seed fixtures (`scenario`) for safe testing and screenshots.

Because a `DraftPackage` is an ordinary CRDT-backed workspace resource, a person, their
teammates, and an agent in Chat can edit the same draft together. Every edit is attributed,
synced across devices, and undoable through standard workspace history. A private prototype
lives in a personal workspace (or a personal `DraftPackage` document) until its author chooses
to share or publish it.

### 1.2 Promoting a `WrittenView` into a package

A person starts either from scratch ("build an expense splitter for this workspace" in Chat,
creating a new `DraftPackage`) or by promoting an existing `WrittenView`:

1. Invoke **Make into app** (`promote`) on a `WrittenView` in Chat or Views.
2. The system creates a `DraftPackage`, wraps the view in an `app` header with a draft
   workspace `AppId`, converts the implicit workspace-app references into explicit `use`
   imports, and generates initial `app.ids` entries.
3. From there, the person or agent can add `type`, `root`, `action`, `initial action`,
   `migration action`, `trigger`, and `scenario` declarations in the same conversation or in a
   code editor window beside the live preview.

### 1.3 Live reload without losing data

Every edit to a `DraftPackage` triggers an in-memory compilation (`tilolang::compile` →
`app.wasm`, `PackageMeta`, updated `IdsLock`), typically completing in a few milliseconds:

- **Valid edit**: the runtime swaps in the new compiled module and re-evaluates open preview
  views immediately. Personal view state (`state`, scroll, focus, selection) is preserved
  whenever the declared `state` types still match.
- **Invalid edit**: the previous valid build stays live so the preview never flashes blank;
  structured compiler diagnostics (`Diagnostic`) attach inline in Chat and the editor.

Iteration supports two data modes, switchable from the preview bar:

| Mode | Backing data | When to use |
|---|---|---|
| **Scenario** (default for new drafts) | Isolated, disposable in-memory document seeded from a declared `scenario` (or empty) | Prototyping schemas, testing edge cases, letting an agent iterate freely without touching workspace state |
| **Live data** | The draft's own workspace document (`<workspace>#app:<draft-id>`) | Dogfooding with real entries before publishing |

Keeping data intact and reversible across edits relies on `app.ids` and CRDT history:

- **Renames**: renaming a type, field, or action through the editor or agent rename action
  updates `app.ids` in place (the equivalent of `tilolang ids --rename`), keeping the 8-byte
  IDs so existing data stays attached without a migration.
- **Additive changes**: new fields with defaults take effect immediately on existing resources.
- **Removals**: removing a field moves its ID to `# retired` in `app.ids` (never re-issued).
  Existing bytes remain untouched in the document rather than being deleted.
- **Incompatible type changes**: if a field's type changes incompatibly (for example `Text` to
  `Int`), the compiler refuses to hot-swap against live data until the author or agent either
  reverts the change or adds a `migration action` (`authority.md`, "Versions").
- **Reversibility**: because both the `DraftPackage` edits and any live-data actions are
  CRDT-backed, undoing a broken schema edit or rolling back to an earlier draft revision
  restores source, `app.ids`, and compiled WASM together.

---

## 2. Package format and publish-time checks

Whether published to a workspace or to a public registry, a tilolang app ships as a single,
architecture-independent `.tapp` package (`crates/core/tilo-app-package`):

- **Signed manifest (`tapp.toml`)**:
  - `AppId`, semantic `version`, optional `previously` (for verified renames per
    `authority.md`).
  - Platform-independent tilolang target — the exact same bytes execute on Tilo OS, macOS,
    Windows, Linux, Android, and Web.
  - Compiler-computed capability summary: declared effects (`uses web`, `uses notify`,
    `uses time`, `uses random`), cross-app `use` dependencies (with their `AppId`s and required
    declaration IDs), declared `trigger`s, and `resources: .own` vs. shared workspace mode.
  - Publisher public key and Ed25519 signature over the manifest and payload root hash.
- **Payload**:
  - `app.wasm` — compiled model, actions, and views.
  - `meta.json` — `PackageMeta` (types, actions, views, search filters, scenarios).
  - `app.ids` — declaration ID lock.
  - `locales/*.ftl` — Fluent translation catalogues (`i18n.md`).
  - Optional `store.json` and screenshot assets (for public catalogue pages).
  - Optional `.tl` source files (required when the package exports `template view`s, and
    recommended so people and agents can inspect what a package does).

### Compatibility checks enforced before signing

Before Tilo signs a new version, the compiler validates it against the previous published
version (from the workspace's package list or the registry):

1. **`app.ids` monotonicity**: no retired ID may be reused; no active ID may silently change
   type without a `migration action`.
2. **Version monotonicity**: the new semantic version must be strictly greater than the latest
   published version for that `AppId`.
3. **Static effect and dependency diff**: the compiler computes every added effect (`uses web`,
   `uses notify`, new `use` imports, new `trigger`s) using `crates/lang/tilolang/src/effects.rs`
   and presents the diff in plain words on the publish confirmation sheet.
4. **Scenario and check pass**: all declared `scenario`s compile and evaluate cleanly;
   required `store.json` fields and `alt` descriptions validate for public releases.

---

## 3. Publishing from inside Tilo

Publishing is a single system-drawn sheet launched from the `DraftPackage` window or from Chat
("publish this to the workspace" / "publish this publicly"). Neither an app nor an agent can
draw or auto-confirm the sheet: publishing always requires an explicit `Approval` from the
person on their own device.

### 3.1 Workspace-only publish

Workspace-only publish lets any authorized member ship an app directly to the current workspace
with no external registry account, no domain DNS proof, and no external review queue.

- **Namespace and `AppId`**: uses a workspace-scoped ID derived from the workspace and a
  human-chosen slug, `ws.<workspace-short-id>.<slug>`. This namespace is reserved by the system
  and cannot collide with global reverse-DNS IDs (`com.example.*`, `so.tilo.*`).
- **Signing and authorship**: signed by the publishing member's device key (certified by their
  person root key). The member is the package's code author for `authority.md` checks.
- **Storage and sync**: the signed `.tapp` is stored as a `WorkspacePackage` resource inside
  the workspace's document tree, with its immutable payload blob synced over the workspace's
  existing peer network. It works offline and on a LAN with no external server.
- **Roles and visibility**: governed by workspace roles and `action-policy`:
  - Publishing, upgrading, or retiring a workspace package requires the workspace's publish
    grant (by default workspace owners and admins, or members granted
    `workspace.packages.publish`).
  - Once published, the app appears immediately in search, the launcher, and "New…" for every
    member of that workspace, governed by ordinary workspace resource permissions.
- **Upgrades and retirement**:
  - Publishing version `0.2.0` checks `app.ids` compatibility against `0.1.0`, re-checks any
    dependent workspace views or packages (`authority.md`), and advances the workspace's pinned
    version.
  - Retiring a workspace package stops new launches and hides it from search, while existing
    resources remain intact and readable in history or through fallback/export views.
- **Graduating to public**: a workspace app can later publish publicly under a proven
  reverse-DNS `AppId`. If existing workspace data should carry over seamlessly, the public
  manifest may declare `previously: ws.<workspace-short-id>.<slug>` co-signed by the workspace
  owner so the workspace adopts the public package over its existing document without copying
  data.

### 3.2 Public publish

Public publish sends the package to `apps.tilo.so` (or any configured registry,
`docs/registry.md`) directly from inside Tilo, using the same cryptographic trust model as
`tilolang publish`.

- **Publisher key**: uses the person's Ed25519 publisher key stored in the local secret store
  (shared with App Publisher and `tilolang`; never written to a synced workspace resource, per
  `docs/publishing.md` Decision 13). If no key exists yet, the publish sheet creates one and
  walks the person through exporting a backup before proceeding.
- **Namespace proof**: public `AppId`s use lowercase reverse DNS with at least three labels
  (e.g. `com.example.budget`, `io.github.ada.budget`). The first time a person publishes under
  a namespace, the publish sheet displays the exact proof line (`tilo-publisher=<public key>`)
  for DNS TXT (`_tilo-publisher.<domain>`), `.well-known/tilo-publisher.txt`, or a public
  `tilo-publisher` repository on a supported git forge (`docs/publishing.md`), verifies it live
  against the registry, and saves the approved namespace state.
- **Store page**: the publish sheet includes a form (which an agent can pre-fill from the app's
  types and doc comments) for `store.json` (`name`, `tagline`, `description`,
  `supported_languages`, `images` with required `alt` text, `source_url`, `homepage`).
  Screenshots can be captured directly from declared `scenario`s via the headless renderer.
- **Signing, upload, and review**:
  1. Tilo packs the `.tapp` and checks `app.ids` compatibility against the latest approved
     entry in the registry.
  2. The system publish sheet displays the exact release summary: `AppId`, version, bundle
     size, capability/effect diff, and target registry.
  3. After human `Approval`, Tilo signs the `.tapp` and the postcard-encoded
     `EntryChangeRequest` (`ENTRY_DOMAIN`, `docs/registry.md`) with the publisher key and
     submits `PUT /uploads/<blake3>.tapp` followed by `POST /entries`.
  4. The submission stays staged privately until the registry owner (or delegated offline
     signing key) verifies the namespace proof, package bytes, and metadata and advances the
     signed snapshot. Status (`queued`, `approved`, `changes requested`) updates inside Tilo.
- **Upgrades and renames**: every subsequent release must be signed by the same publisher key
  and pass `app.ids` compatibility. Renaming a public app uses `previously:` signed into the
  manifest (`authority.md`), verified by the registry to belong to the same publisher key.
- **Yanking and revocation**:
  - *Yank*: the publisher signs a removal `EntryChangeRequest` so the version leaves the active
    index. New zero-install opens will not select it; workspaces already pinned to it see a
    non-destructive upgrade notice.
  - *Revocation*: for a security defect or compromised key, the registry owner (or the
    publisher via a signed revocation) appends a revocation entry to the registry's moderation
    head. Devices receiving the next snapshot refuse to load the revoked package hash even if
    it is already in local cache, while keeping the workspace's own resources intact and
    inspectable.

---

## 4. Zero-install use

Opening a tilolang app requires no install wizard, no confirmation dialog before seeing the UI,
and no restart.

### 4.1 How a person opens an uncached app

Any of four gestures triggers on-demand loading:

1. **Search / command palette**: type an app name or action from the registry catalogue or the
   workspace's package list and press Enter.
2. **Link**: follow a `tilo://` or HTTPS link pointing to an app or a specific resource.
3. **Shared resource**: open a workspace or shared resource whose `ResourceType` belongs to a
   package not yet present on this device.
4. **Cross-app `use` / embedded view**: open a view that composes a type from another package.

### 4.2 Fetch, verify, compile, execute

The session resolves and loads the package through one pipeline:

1. **Resolve**:
   - Check the local content-addressed package cache first.
   - For `ws.*` IDs or apps already pinned by the workspace, resolve from the workspace's
     `WorkspacePackage` list (and blob store).
   - Otherwise resolve from the configured registries' cached `CompactRegistrySnapshot`
     (`GET /entries?format=compact`, revalidated with `ETag`).
2. **Fetch**: download the compressed `.tapp.zst` (or workspace blob) under a strict size cap
   for zero-install loads (e.g. 4 MB compressed; typical tilolang packages are 50 KB – 1.5 MB).
3. **Verify before compiling**:
   - Verify the registry snapshot signature against the pinned owner key and anti-rollback
     checkpoint (`docs/registry.md`), or verify the workspace member's device certificate and
     person key for workspace packages.
   - Decompress and verify the BLAKE3 root hash matches the signed entry.
   - Invoke `tilo_app_package::verify` to check the publisher's manifest signature and payload
     hashes.
   - Check the registry's revocation list (or workspace retirement flag).
   - Validate the WASM module's import table: only the tilolang runtime imports are allowed —
     no filesystem, network, process, or raw clock imports exist.
4. **Compile and cache**: compile `app.wasm` in the sandboxed WebAssembly engine
   (`tilo-model-engine` / tilolang runtime), store both the verified `.tapp` and compiled
   artifact in the local cache keyed by BLAKE3 hash, and open the target resource (or the app's
   `root`) in the shell.

### 4.3 Cache eviction and pinning

- **Automatic eviction**: zero-install packages live in a bounded LRU cache on disk. When the
  cache exceeds its size budget, the least recently used unpinned packages are evicted
  automatically. Evicting a package removes only its cached bytes and compiled artifact — it
  never deletes or alters any workspace resource.
- **Pinning**: a person or workspace can pin an app to guarantee offline availability, keep it
  in the launcher, or lock its version. Pinned packages are exempt from LRU eviction and are
  pre-fetched onto every device of the person that holds the workspace's keys.
- **Workspace version pinning**: the first time a workspace writes a resource using a public
  app, the workspace pins that package's exact version and BLAKE3 hash in workspace state.
  Every key-holding device in the workspace then replays CRDT changes with the exact same
  deterministic WASM module (`docs/architecture/apps.md` §1). Updating the pinned version is an
  explicit workspace action that displays the compiler's compatibility check (`authority.md`).

---

## 5. Trust and capability boundary

Zero-install is safe only if opening an unknown app can do no harm by default. Authority
follows `authority.md` and `docs/architecture/apps.md`:

- **Effective authority is the intersection** of the person's authority and the package
  author's authority (`action-policy`), checked at the moment each action executes.
- **Untrusted presentation**: a tilolang app emits only described components (`ui.*`) to the
  shared app window. It cannot draw over system chrome, approval sheets, or identity bars, and
  it never holds keys, tokens, or network sockets.

| Capability | Zero-install default | When explicit approval / grant is required |
|---|---|---|
| Render UI (`ui.*`, `std.*`) | **Allowed** | Never (constrained to described components) |
| Personal view `state` (scroll, sort, selection) | **Allowed** | Never |
| Read & edit the app's own declared types (on the opened resource or a new empty root) | **Allowed** | Never for undoable changes within the workspace's existing member access |
| `uses time` / `uses random` (journaled, deterministic on replay) | **Allowed** | Shown in package inspector; cannot leak or forge replay state |
| `uses web` (outbound HTTP / outside service) | **Ask** | System approval sheet naming exact destinations/endpoints before any request |
| `uses notify` (sending notices to members) | **Ask** | System approval sheet before the first notice |
| `uses clipboard` (copying to, and pasting from, the person's clipboard) | **Ask** | System approval sheet naming what the app copies and reads before the first copy or paste |
| `uses image` (decoding, cropping, turning, resizing and encoding the app's own files, and its previews) | **Allowed** | Never: the pixels are of files the workspace already holds, and the cache is the device's own |
| Reading another app's resources or calling its actions (`use`) | **Ask** (stranger's code has no foreign grants) | System approval sheet showing the actual target resource and action, never the app's button label (`authority.md` §6) |
| Background `trigger`s (`every` / `at`) | **Disabled** until pinned/enabled | Workspace must explicitly enable the app's triggers with a grant listing schedules and effects |

Additional invariants:

- **No silent writes on open**: opening a zero-install app that uses `resources: .own` or a
  workspace `root` does not create a persisted document until the person actually invokes an
  action or edits a field (previewing an empty app leaves zero workspace clutter).
- **Grant pinning by hash**: every grant is bound to the exact package hash (`authority.md`
  §5). If a newer version adds `uses web`, a new `use` dependency, or a new `trigger`, the
  system prompts for the delta before activating the update.
- **Visible provenance**: the window's info sheet always displays the `AppId`, version, BLAKE3
  hash, publisher identity (verified domain/forge or workspace member name), declared effects,
  and active grants.

---

## 6. Relationship to CLI and native publishing

| Aspect | Native / Rust apps (`docs/publishing.md`) | tilolang apps (this doc) |
|---|---|---|
| Authoring | Rust workspace + `tilolang` | Chat / in-Tilo editor (`DraftPackage`), or `.tl` files in git |
| Build | 10-target CI matrix (`tilo-builder`, macOS, Windows, Android, Web) | Single platform-independent WASM `.tapp` compiled inside Tilo |
| Workspace-only publish | Not supported without a self-hosted registry | First-class: signed `WorkspacePackage` resource synced over peer network |
| Public publish | `tilolang publish` / CI on `v*` tags | In-Tilo publish sheet (same publisher key, registry API, and review flow); CLI/CI also supported |
| Client adoption | Explicit install of platform binary | Zero-install on-demand fetch, verify, compile, and LRU cache; optional pin |

Teams that prefer git repositories and CI can still check `.tl` files and `app.ids` into a
repository and invoke `tilolang package` / `tilolang publish` with the same `.tapp` format and
publisher key.

---

## 7. Open questions

1. **Hot-reload cadence for shared drafts**: when two members view a `DraftPackage` while an
   agent edits it in Chat, should the other member's window reload automatically on every valid
   compile, or show a quiet "Updated — reload" bar so live gestures are never interrupted?
2. **Review tiering for pure tilolang packages**: `docs/publishing.md` Decision 8 queues every
   public release for owner review. Should pure tilolang packages with no `uses web` and no
   cross-app `use` dependencies qualify for automated verification and faster publication, or
   keep manual review for all public catalogue entries?
3. **Personal public namespace without DNS or a forge**: public publishing currently requires a
   proven domain or git forge account (`docs/publishing.md`). Should Tilo provide an
   identity-bound namespace (such as `so.tilo.u.<handle>`) so a person can publish publicly
   from inside Tilo with no external DNS or forge setup?
4. **Mandatory vs. optional `.tl` source in public `.tapp`s**: packages exporting
   `template view`s must carry template source for caller expansion. Should all public tilolang
   packages include their full `.tl` source for auditability and agent inspection, or allow
   WASM-only publication when no templates are exported?
5. **Transitive `use` dependencies in offline workspaces**: when a workspace-only package
   `use`s a public library package, should publishing to the workspace vendor the library's
   signed `.tapp` blob into the workspace's blob store so offline members can always resolve
   it?
6. **Sharing a single resource from a workspace-only app outside its workspace**: if a person
   shares one resource created by a `ws.*` package with someone outside that workspace, should
   the recipient fetch the signed `WorkspacePackage` directly over iroh from the sharing peer,
   or must the app be published to a registry first?
