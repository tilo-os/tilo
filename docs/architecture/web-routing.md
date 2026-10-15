# Links and web routing

A Tilo link names one thing in one room: a workspace, an app open in it, or a
resource inside that app, optionally at a range of its text. The same link
opens the whole desktop or just that app in a full browser tab, works in a
browser with nothing installed, opens in an installed Tilo, survives Back and
Forward and reload, and is what a person, a bot or an agent gets when they
ask for "a link to this".

A link is not a grant. It is a rendering of the address the action model
already uses (`ResourceRef` plus the record provider and the declared view,
[resources.md](resources.md)), resolved after admission through the same
permission-filtered catalogue every other caller uses. Whoever opens it still
has to be a member of the room, be admitted to it, or hold an invitation.

This document is the contract for the link grammar, the web entry pages
(`crates/browser/tilo-browser-runtime/web`), the native opener, and the
`share-link` action. The page API it builds on is in
[browser-runtime.md](browser-runtime.md).

## 1. What exists today

Several partial mechanisms grew independently. This design replaces them with
one grammar; it reuses their checks, not their encodings.

| Mechanism | Where | What it does | What changes |
|---|---|---|---|
| Version-one resource link | Was `web/resource-link.mjs`, `web/resource.html`, `web/resource-page.mjs`, minted natively by Chat's `copy-resource-link`/`open-resource-link` as `resource.html#resource=<percent-encoded JSON {version, origin, room, hostDevice, provider, action, reference}>` | Removed. Its checks live on as the resolver (`resolveResourceReference`, `openResourceReference` in `web/resource-reference.mjs`); links use the grammar below and enter through `index.html` or `app/index.html`. No web host serves `resource.html`, and Cloud's `/qos-runtime/resource.html` entry is not part of any release |
| Locator | `ResourceViewLocator` in `crates/collab/actions/src/remote/catalogue.rs`, filled by `tilo-relay` from its `web_entry` setting (`--web-entry`, `TILO_ROOMS_WEB_ENTRY`) | Host-issued `{origin, room, hostDevice, provider, webEntry}` on a catalogue page | Was `browserPage`; `webEntry` is the host's web entry base (§3.1); the rest is unchanged |
| Diagnostic and dev entry | `web/index.html` (`?app=`, `?room=`, `?workspace=`), `tilo-app build --platform web` (`index.html?app=<id>`), `tilo-web-desktop/scripts/smoke.mjs` | Mounts one app or the desktop from query parameters | Same grammar under the `app/` entry (§3.3); query strings carry diagnostics only |
| Room service join link | `crates/hosting/tilo-room-service` (`guest_link`, `entry.json`), `web/rooms.mjs` (`createInvitation`) | Was `/app/join.html#origin=…&user=…&room=…&ticket=…&admit=…[&record=…]` | Now `/open/app/#/@<relay>/w/<room>/so.tilo.word-processor[/<record>]?ticket=…`: app mode with a `ticket` secret (§6), redeemed at the door `entry.json` names |
| Invitations | `peer.createInvitation`, `inspectInvitation`, `joinWorkspace` (browser-runtime.md, "JS API Cloud targets"); `/join` steps in `crates/hosting/tilo-relay-protocol/src/rooms.rs`; capability tokens in `crates/collab/collab-types/src/invite.rs` | Base64url invitation text redeemed by a device; the owner admits, or a pre-admission token admits at once | Carried as the `invite` fragment secret (§6) |
| Desktop navigation | `web/navigation.d.ts` (`DesktopNavigationV1`), `tilo_desktop_surface::navigation` | Workspace/strip/group/tab routes, per-tab back and forward, `launch` | Gains the focused window's location and an `open` command (§5) |
| Record URI | `collab_types::RECORD_URI_SCHEME` (`tilo-record:<record id>`), launcher `--open-uri` (`crates/packaging/launcher/src/run.rs`) | Asks an app to open one of its own records at launch | Unchanged: it is the in-process launch form of a link's record segment |
| URI opener | `tilo_dialogs::open_uri` and the host `UriOpener`; `ui::widget::try_open_external_url`; link detection in `ui-core/src/text_detect.rs` | Hands any URL to the platform | The native opener intercepts Tilo links first (§7) |
| Ad-hoc `tilo://` strings | Photos copies `tilo://resource/<id>`; MCP resources `tilo://apps/<peer>`, `tilo://application/<app>`; pairing QR `tilo://invite/<base32>` (`tilo-identity-ceremony`) | Each its own shape; nothing resolves the Photos one | Photos uses `share-link`; MCP resource URIs that name room resources render this grammar (§7.3); the pairing payload stays a pairing payload |

The architecture overview already asks for this: "URLs or intents for
everything, above and below file granularity. Opening something is a record
and actions, not a second launcher" ([README.md](README.md)).

## 2. Decisions

1. **The route lives in the fragment; the path names only the entry and the
   mode.** Pages are served by a web host (Cloud, `tilo.so`, the room
   service's `/app/`) that is not the room's host and must never learn which
   rooms, records or resources a person opens. Sealed routes already hide
   content from relays; a path or query would hand resource ids (file paths,
   channel names) to access logs, CDN analytics and `Referer`. The fragment
   never leaves the browser. It also means one immutable bundle serves every
   route without rewrite rules, which is how Cloud and the room service host
   it today. The cost is that link unfurlers see only the entry, so private
   links never get previews; that is the right default for private rooms, and
   the web app's entry carries generic static Open
   Graph tags (§8).
2. **Mode is a path segment: `app/` or nothing.** Desktop mode and full-tab
   app mode show the same address, so the fragment is identical and switching
   mode keeps it. Two finite, public values in the path reveal nothing, let
   the app entry load less than the desktop (the web build's size budget), and
   let a host serve each as a static file. A fragment flag would make the mode
   part of the address, which it is not.
3. **The URL carries the address and nothing else.** Room, app, record,
   resource kind and id, span, and a non-primary view. No labels as truth, no
   layout (workspace/strip/group/tab ids are room state salted per device and
   mean nothing to a recipient), no scroll offsets or caret. What should be
   linkable is published as a resource; an app that wants a heading or a page
   linkable publishes it, rather than inventing a URL parameter.
4. **One parser, in Rust.** The grammar lives in `tilo-reference` beside
   `ResourceRef` and is exported to the page through the runtime's wasm
   bindings, so native, browser, relay and agent code cannot disagree. The
   one JavaScript exception is the secret scrubber (§6), which must run before
   any module loads and is tested against the same corpus.
5. **The host device key is not in the link.** The version-one link pinned
   `hostDevice`. Admission already verifies the host's certificate against
   the room owner's root, so a link retargeted to another host cannot be
   served by it; the pin added 70 characters and no protection. A link that
   names a host and room only ever reaches the room's certified host.
6. **Sharing is an action.** Every resource with a declared view publishes
   `share-link`, a framework action whose output is the URL. The window menu's
   Copy link, chat's message chip, a bot posting to another service and an
   agent answering "send me a link" all invoke it; nothing formats URLs by
   hand.

## 3. Grammar

### 3.1 Entries

A web entry is an HTTPS base URL ending in `/`, served by a web host:
`https://tilo.so/`, a Cloud console's runtime path, a relay serving its own
bundle (`https://machine.example:8443/`). A host's catalogue locator
announces its entry (`ResourceViewLocator.webEntry`, configured by the
operator); minting uses the entry the host announced, never one guessed from
the API origin.

No entry stands for a room: every link names its workspace's host and room,
so a public workspace's links have the same shape as a private one's. A web
host may name a **home workspace** that its bare entry (no route) opens
instead of the person's own desktop (`entry.json` `home`, §8); that is
configuration outside the grammar, and the page rewrites the address to the
home workspace's full link once admitted. There is no global room namespace.

`http:` is accepted only for `localhost`, `127.0.0.1` and `[::1]`, as
`resource-link.mjs` already enforces.

### 3.2 Syntax

```abnf
link        = entry [ "app/" ] [ "#" route ]
route       = [ scope ] [ target ] [ "?" params ]

scope       = "/me"                                   ; the person's own room
            / [ "/@" host ] "/w/" [ slug "~" ] room   ; a workspace room
host        = [ "https:" ] reg-name [ ":" port ]
                                          ; HTTPS origin of the room's host;
                                          ; absent: the page's own origin.
                                          ; Loopback names are http unless
                                          ; written "https:" (a test relay
                                          ; with its own certificate)
slug        = 1*48 ( %x61-7A / DIGIT / "-" )          ; presentation only
room        = uuid                        ; lowercase, hyphenated (RoomId)

target      = "/" app [ "/" record [ "/" resource ] ]
app         = package-id                  ; always the full id, e.g. so.tilo.chat
record      = "-" / segment               ; "-": the package's primary record
resource    = segment                     ; id; kind = text before its first ":"
            / segment "/" segment         ; kind "/" id

params      = param *( "&" param )        ; canonical order as listed
param       = "view=" segment             ; a declared view id, when not primary
            / ( "chars=" / "bytes=" ) 1*DIGIT "-" 1*DIGIT   ; Span
            / secret                      ; §6

segment     = 1*pchar-without-slash       ; RFC 3986 pchar, "/" percent-encoded
```

Rules that keep the mapping one to one:

- **App.** Always the full package id, first-party or not (`so.tilo.chat`,
  `com.example.tasks`): no app gets a shorter or privileged spelling. Package
  ids contain a dot, so an app segment never collides with the scope tokens
  `me`, `w` or `@host`.
- **Record.** Every `RecordId` has a prefix ending in `:` (`uuid:`, `file:`,
  `tab:`, `derived:`, the file-record prefix), so `-` never collides. `-`
  is the wire's empty record, `RecordId::package_primary()` in the store.
- **Resource.** The formatter emits one segment when the id starts with
  `<kind>:` (`chat.channel:general`, kind `chat.channel`) and two otherwise.
  Three target segments after the app parse as id-with-kind, four as kind and
  id; a one-segment id without `:` is refused.
- **Slug.** Derived from the verified workspace name when the link is
  minted, ignored on parse, and rewritten in place (`replaceState`) once the
  page has resolved the room, so a renamed workspace's links fix themselves.
  Like `ResourceRef.label`, nothing resolves, compares or routes on it.
- **Span.** `chars=` or `bytes=` is `SpanUnit`; start ≤ end, both within
  JavaScript's exact integer range (as the chat minting already checks). A
  span requires a resource and a view with a range binding.
- **Strictness.** Parsing refuses rather than repairs: uppercase or
  unhyphenated uuids, unknown parameters, duplicates, parameters out of
  order, a missing scope.
  A link from a newer bundle says it needs a newer Tilo instead of silently
  dropping what it does not understand.
- **Bounds.** The route is at most 8 KiB, the launcher's `--open-uri` bound;
  segments obey the existing limits (record ids, 2048-byte resource ids,
  128-byte kinds).

The query string (`?…` before `#`) is not part of the grammar. It is kept for
non-sensitive diagnostic switches such as the WebGPU override read by
`web/webgpu-safety.mjs`, and the formatter never writes one.

### 3.3 Examples

```text
https://app.example/#/@rooms.example/w/commons~0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001
    A hosted workspace's desktop in the Tilo app on the web.

https://app.example/app/#/@rooms.example/w/0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001/so.tilo.chat/-/chat.channel:general
    Its Chat, full tab, on #general.

https://app.example/#/@rooms.example/w/commons~0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001/so.tilo.chat/-/chat.message:01JA2C7Q
    A message with Chat focused on its desktop (what a bridge bot posts).

https://tilo.so/#/@quinten.tilo.cloud/w/reading-group~6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90/so.tilo.word-processor/uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f/word_processor.block:b12?chars=40-96
    A passage in a private workspace's document, on the desktop.

https://tilo.so/app/#/me/so.tilo.calculator
    Calculator full tab in the person's own room; what `tilo-app dev` opens.

https://tilo.so/app/#/@quinten.tilo.cloud/w/reading-group~6f1c…/so.tilo.word-processor/uuid:3e0d…?invite=AAECAwQF…
    The document full tab, with an invitation for someone not yet a member.

tilo://open#/@quinten.tilo.cloud/w/reading-group~6f1c…/so.tilo.chat/-/chat.channel:general
    The same channel, handed to an installed Tilo (§7).
```

### 3.4 Mapping onto the action model

| Link part | Action model |
|---|---|
| `@host` | `ResourceViewLocator.origin` |
| `w/<room>`, `me` | the room (`RoomId`; the desktop workspace whose `room` it is) |
| `app`, `record` | provider `record:<app>:<record>` (`-` is the empty record) |
| `resource` | `ResourceRef { kind, id }`; the label comes from the live publisher |
| `chars=`/`bytes=` | `ResourceRef.span` |
| `view=` | `ResourceViewSpec.id`; absent means the primary view |
| `app/` | which mount presents it, not what it addresses |

The link names the view by its stable `id`, not by its open action:
`ResourceViewSpec` keeps `open_action` an execution binding that may change,
so a link minted before a rename still opens. The resolver maps the id to the
current `open_action` (and `range.action` for a span) from the fresh
catalogue, exactly as `openResourceReference` does with the action today.

A link with only a scope opens the workspace; with an app and no record, the
app in that room as launching it would; with a record and no resource, that
record's primary view.

## 4. Interfaces

### 4.1 Rust (`tilo-reference`, module `link`)

```rust
/// Where a link points. Rendering it is the only way to make a link.
pub struct LinkTarget {
    pub scope: Scope,
    pub app: Option<PackageId>,
    pub record: Option<RecordSegment>,   // Primary | Id(String)
    pub resource: Option<ResourceAddress>,
    pub view: Option<String>,            // ResourceViewSpec::id, None = primary
}

pub enum Scope {
    Own,                                 // "me"
    Room { host: Option<HostOrigin>, room: RoomId, slug: Option<Slug> },
}

/// `ResourceRef` without its label: what a link may carry.
pub struct ResourceAddress { pub kind: String, pub id: String, pub span: Option<Span> }

pub enum Mode { Desktop, App }

/// Never rendered outside the fragment; `Debug` and `Display` redact.
pub struct LinkSecrets { pub invite: Option<Secret>, pub ticket: Option<Secret>, pub token: Option<Secret>, pub pair: Option<Secret>, pub install: Option<Secret> }

pub struct Link { pub mode: Mode, pub target: LinkTarget, pub secrets: LinkSecrets }

impl Link {
    pub fn parse(url: &str, entries: &WebEntries) -> Result<Link, LinkError>;
    /// `tilo://open#<route>`: the scope is always explicit.
    pub fn parse_native(uri: &str) -> Result<Link, LinkError>;
    pub fn to_url(&self, entry: &WebEntry) -> String;
    pub fn to_native(&self, entries: &WebEntries) -> String;
    /// The same link without secrets: what history and logs may hold.
    pub fn public(&self) -> Link;
}

impl ResourceAddress {
    pub fn from_ref(reference: &ResourceRef) -> Self;
    pub fn into_ref(self, label: String) -> ResourceRef;
}
```

`WebEntries` is the configured set of entries: the bundle's
configuration on the web, the device's known hosts natively. `LinkError`
says which rule failed, in words a person can act on ("This link is
incomplete: it names an app but no workspace").

### 4.2 Page (`web/router.mjs`, `web/host.d.ts`)

```ts
/** Parsed by the runtime's Rust parser; secrets already removed (§6). */
export interface LinkTarget {
  scope: {kind: 'own'} | {kind: 'room'; host?: string; room: string};
  app?: string; record?: string /* '' for the primary record */;
  resource?: {kind: string; id: string; span?: {start: number; end: number; unit: 'Chars' | 'Bytes'}};
  view?: string;
}
export interface Route { mode: 'desktop' | 'app'; target: LinkTarget }

export function parseLink(url: string): Route;
export function formatLink(route: Route, entry?: string): string;

/** Owns location and history for one page. */
export function startRouter(options: {
  mount: DesktopMount;
  secrets: LinkSecretsHandle;   // from the scrubber, read once by admission
  onRoute?: (route: Route) => void;
}): {navigate(route: Route, how: 'push' | 'replace'): void; stop(): void};
```

`DesktopNavigationSnapshot` gains `location: LinkTarget | null`, the focused
window's (desktop) or the app's (app mode) current address with its cause:
`{target, cause: 'open' | 'focus' | 'remote'}`. `DesktopNavigationCommand`
gains `{kind: "open"; target: LinkTarget}`, which invokes the target's view
action through the door, as a palette or search hit does, so placement and
presence follow [resources.md](resources.md) §4. The single-app mount is the
existing `mountDesktop(element, {app, record, peer})` followed by the view
action, which `openResourceReference` already performs.

### 4.3 `share-link`

The framework publishes two actions on every resource that declares at least
one view; a resource with no view has nothing to open, so it has no link.

| Action | Kind | Parameters | Output |
|---|---|---|---|
| `share-link` | Read | `view?` (view id), `start?`, `end?`, `unit?` (only when the view has a range), `mode?` (`app` or `desktop`; default below) | `{url}` |
| `share-invite-link` | Outside | as above, plus `role` (`viewer` default, `commenter`, `editor`), `expires_in?` | `{url}`, with the invitation in the fragment |

- The minting device asks its host for a fresh locator, exactly as
  `navigation_link` in chat does today, and refuses when the host announces
  no web entry ("This workspace's host serves no web page") rather than
  guessing one.
- `share-link` needs Read on the resource. It mints no capability, so it is
  Read even though the URL leaves the device; the link is useless to anyone
  who could not open the resource anyway.
- `share-invite-link` creates a redeemable invitation through the room's
  membership door (`peer.createInvitation` on the web, the same `/rooms`
  request natively), so it needs the room's invite grant
  ([permissions.md](permissions.md)) and is an outside consequence: it is
  recorded in the room's history and can be cancelled from the membership
  list.
- When a person invokes either through a view, the view copies the URL to
  the clipboard and says so; when an agent or bot invokes it, it gets the URL
  as output. Same action, same contract, visible in history as "Quinten
  copied a link to #general" or "Discord bridge shared a link to #general".
- `mode` defaults to `app` for a link to a resource or an app: a recipient
  wants the thing first, may not be a member of the room whose desktop the
  other mode would show, and the full tab offers "Open in desktop" with the
  same fragment. A link to a bare room (a workspace) defaults to `desktop`,
  since there is no single app to show.
- Chat's `copy-resource-link` becomes `share-link` on the message's
  referenced resource; `open-resource-link` becomes the resource's view
  action. Following a reference inside Tilo never needs a URL.

## 5. History, Back and Forward

The URL always says what is in front of the person; the history stack holds
what they navigated to.

- **Push** when this person opens something: a link, a search hit, a palette
  entry, a reference chip, a channel in Chat's sidebar. Concretely, a local
  invocation of a View action that changes what the focused window shows, or
  opens a window (`cause: 'open'`).
- **Replace** when the focus moves between windows already open
  (`cause: 'focus'`), when another device's write moves it (`cause:
  'remote'`), and to fix the slug or drop secrets after resolution.
- **Never** for selection, caret, scroll or zoom: a span enters a URL only
  through `share-link`. Agents' and collaborators' actions open views on
  their own strips ([resources.md](resources.md) §4) and never push an entry
  or change this person's URL.
- **Back and Forward** (`popstate`, and `hashchange` when someone edits the
  fragment by hand) parse the URL and dispatch `open` for it. A window still
  showing the target is selected; a closed one is reopened through its view
  action. The history entry's state object is only a page-generation key;
  the URL is the whole truth, so reload, Back and a pasted link take the same
  path.
- **App mode** has one window, so the browser's history is that window's
  history: the runtime's per-tab back and forward
  (`DesktopNavigationSnapshot.history`) is driven by `popstate` and the app's
  own back control calls `history.back()`. In desktop mode browser history
  spans windows ("where was I") and each window keeps its own back and
  forward in its chrome.
- **Mode switch.** The desktop window menu's "Open in its own tab" opens the
  `app/` URL in a new tab and leaves the desktop as it is; the full tab's
  "Open in desktop" assigns the desktop URL in the same tab. Both keep the
  fragment.

`web/host.mjs` already keeps horizontal wheel gestures from reaching browser
history; that stays, so a sideways swipe pans the desktop instead of leaving
the page.

## 6. Admission and secrets

### 6.1 Opening a link without access

1. **Parse.** A malformed link stops here with the parser's message.
2. **Scrub** secrets (§6.2).
3. **Resolve the scope** to a host origin and room, from the route, or for
   a bare entry the web host's home workspace (whose room is learned on
   admission).
4. **Identity.** A browser account for that host (`web/browser-accounts.mjs`)
   is offered first; the picker reads Settings' personal accounts record. Without one: an invitation,
   a ticket or a host with an admission door lets this browser join as the
   person it has, or a new one made on the spot (`joinWorkspace`, `joinRoom`); otherwise the page offers
   the usual sign-in and keeps the link to continue afterwards.
5. **Admission.**
   - A member connects (`connectPeer` with an existing certificate).
   - With `invite`, the page first shows what `inspectInvitation` verified
     (workspace name, role, inviter), then asks to join; the owner admits
     (`asking`, `waiting`, `admitted`) or a pre-admission `token` admits at
     once. The pending request is kept in the profile, so a reload while
     waiting continues it even though the secret is gone from the URL.
   - With `ticket` (room service rooms), the page shows its pairing code and
     the ticket is spent on it at the redemption door the web host's
     `entry.json` names (`tickets`); a host that names none refuses the link.
   - With `pair`, the page becomes a new device of that person: it shows its
     pairing code, the person approves it on a device that holds their root
     (the desktop's "Pair a new device"; the host relays the request and
     checks the root's signature, `/certify/approve`), and the page connects
     as them with the certificate the root signed. Nothing is asked of any
     server but the host the two devices share.
   - A host whose **listed workspace has an admission door**
     (`web/workspaces.mjs`: a workspace is
     `{name, host: {origin, key?}, room?, admission?}`, and `admission` is
     the host's door) admits this browser's person at once and as Editor,
     without a secret: `joinViaHost` takes the person's identity (the page
     owns creating and keeping it), `POST <admission>join` pre-admits it and
     the page redeems the bound invitation. A person who joined that host
     before reconnects. A link naming another room on that host than the one
     the person was admitted to reads as no access. This is an admission policy of the host, not a
     separate page: a web host offers such a workspace as its home (§8).
   - A host this browser set up for its person (§6.3), or joined as them
     through an invitation, is reconnected to with the root the page holds
     (`reconnect`, `hostToReconnect` in `web/workspaces.mjs`): on a machine
     of their own the host first admits this device as the owner of the
     room the link names (`workspaces.enter`), then the page connects to
     it. The host decides; a refusal reads as no access, like the rest.
   - Otherwise the page says the person has no access and that the person
     who shared the link can invite them. It never says whether the record or
     resource exists: as the action door does with `Unknown`, "not there" and
     "not for you" read the same.
6. **Resolve the target** through a fresh catalogue read
   (`resolveResourceReference`); labels and view contracts come from the
   live publisher, never the link.
7. **Present** in the link's mode, then `replaceState` the canonical public
   URL.

### 6.2 Secrets

Secrets are `invite` (invitation text), `ticket` (a room service guest
ticket), `token` (a pre-admission token), `pair` (the user root, hex, of
the person whose new device this browser asks to become) and `install` (a
machine waiting for its owner, §6.3). Rules:

- They only ever appear in the fragment. `LinkSecrets` has no path or query
  rendering; there is no API that could put one there.
- The entry HTML's first, inline, classic script reads `location.hash`, moves
  the secrets into page memory and calls `history.replaceState` with the
  public URL before any module loads. Copying from the address bar, a crash
  report, a screenshot of the address bar or a later `share-link` never sees
  them. It is the one JavaScript parser, and it only splits off the five
  known keys; the corpus tests (§9) hold it to the Rust parser's split.
- Secrets are read once by admission and dropped after use.
- Entry pages set `<meta name="referrer" content="no-referrer">` and load
  no third-party script that could read `location`.
- Logs, diagnostics, history entries and the native opener use
  `Link::public()`; `LinkSecrets` redacts in `Debug` and `Display`.
- Room service links no longer carry the room's user key or admit endpoint:
  the page learns both from the room host's announcement, so a guest link is
  just a route and a `ticket`.

A link without secrets is safe to post anywhere: it says which room and
resource exist, which the room's members already know and which the host's
admission check makes useless to anyone else. A link with a secret is a
capability and should be sent as one; the share sheet says so when
`share-invite-link` is used.

### 6.3 Setting up a machine

A node provisioned without a certificate (`tilo-relay run --web …
--install-token-file PATH`, see browser-runtime.md, "A browser-held root")
waits for its owner: it prints its device key and answers only its install
route. Making it the person's is a host action of the desktop and a link
form, both in `web/machine-setup.mjs`; neither is admission, so `install`
never opens a room and admission never reads it.

- **"Set up a machine"** (`set-up-machine`) is a host action on every
  desktop the page mounts, listed in desktop search beside the identity
  actions. It asks for the machine's HTTPS address, its key (the
  `devicePublicKey` it printed) and its install token.
- **The link** is `#/me?install=<secret>`: the secret is base64url of the
  JSON `{origin, devicePublicKey, installToken}` (`installSecret`). It obeys
  §6.2 like every secret: the scrubber takes it before any module runs,
  `boot` hands it to the page instead of admission, and the page reads it
  once. A damaged one is said, never acted on.

Either way the page shows the same form, filled in from the link, with what
setting up means, and does nothing until the person presses "Set up": a
link alone never makes a person certify a machine. The root this page holds
(the one unlocked, else this browser's own, made on a first visit) then
certifies the node and the session beside it and installs both
(`installNode`), while the form says each step. A wrong token, a machine
already set up (a serving host has no install route), a key that is not
the machine's, an unreachable machine or one that does not allow this page's
origin, and a browser that holds no root of the person it is (a device
paired to someone kept elsewhere) are each said in the form, which stays
open to correct and try again. A machine that took the certificate is
remembered as a host the person owns (`keepOwnedHost` in
`web/workspaces.mjs`): like a host they joined, it keeps their envelope and
profile.

### 6.4 Workspaces and invitations from the desktop

`web/workspace-actions.mjs` adds the desktop's host actions for workspaces
on the person's machines, each the host's decision:

- **"New workspace"** (`new-workspace`), on a desktop whose host serves the
  person it is (its certificate is their root's), or on their own desktop
  once this browser set up a machine for them (also offered right after
  setting one up). The host creates the room and admits this device as its
  owner (`workspaces.create`, `enter`), and the page opens the room's link
  (another room is another admission, so the page loads again).
- **"Invite someone"** (`invite-someone`), in a room, and the desktop's own
  Invite: `createInvitation` as Editor, presented as the room's link with
  its `invite` secret, to select or copy (inside the person's click, as
  Safari's clipboard requires) and send. Opening it is §6.1's `invite`.
- **"Waiting to join"** (`waiting-to-join`), for the room's owner: the
  people who asked, each let in or turned away (`membership.requests`,
  `admit`, `decline`). While the owner's page is in the room it asks every
  few seconds, and offers each new request once on a banner that lets the
  person in.

A person let in through an invitation is remembered for that host
(`keepJoinedHost`), so their next visit reconnects (§6.1).

## 7. Native handling

### 7.1 Tilo's own opener

The shell installs the host `UriOpener` that `tilo_dialogs::open_uri`,
`ui::widget::try_open_external_url` and every detected link go through. Before
handing a URL to the app that claims its scheme (the browser claims
`https` through `[providers] schemes`), it tries `Link::parse` against the
device's known entries (`tilo.so`, each host it is a member of, as announced
in their locators) and `Link::parse_native` for `tilo://open`:

- The room is one of the person's workspaces: dispatch `open` for the target
  through the room's door, on the active strip. App mode opens the window
  maximized.
- Not a member, with `invite` or `ticket`: the native join flow, with the
  same inspection screen.
- Not a member and no secret, or the host is unknown: the URL goes to the
  browser like any other web page, where §6.1 applies.

The URL is handed over in process, so a secret never reaches a command line
or a launcher log.

### 7.2 Installed Tilo on another system

Where an installed Tilo can claim the web entries (Universal Links, App
Links), it receives the https link itself. Elsewhere, it registers the
`tilo` scheme, and the web entry's "Open in the Tilo app" control navigates to
`tilo://open#<route>` with the scope made explicit, which
`Link::parse_native` reads. `tilo://invite/…`
stays the identity ceremony's pairing payload; both sit under one scheme
registration.

### 7.3 Agents and MCP

Agents get links from `share-link` and open them through the `open` command
or the target's view action. MCP resource URIs that name a room resource
(the `tilo://workspace/...` addresses [agents.md](agents.md) anticipates)
render this grammar as `tilo://open#<route>` instead of a parallel scheme.
Process-local addresses such as `tilo://apps/<peer>` name running peers, not
room resources, and stay as they are.

## 8. Workspace hosts and home workspaces

Hosting a person's Tilo (the web app and their identity) is separate from
hosting workspaces. A web host serves the app and the person's session; a
workspace lives on whichever host its data names. A workspace host is named
after the machine, not after a workspace (`rooms.example`), serves many
workspaces, and serves its room API and relay on one origin; it serves no
app page of its own, and its `GET /` leads to the web entry it is configured
with.

A workspace whose host lets people in without an invitation carries the
host's door as `admission`. Opening it without membership joins it through
that door (§6.1) from inside the app; afterwards it is an ordinary
workspace, and its links name it like any workspace
(`https://app.example/#/@rooms.example/w/commons~<room>/…`). A host keeps a
persistent room's id across restarts, so links survive them.

A web host may name a **home workspace** in `entry.json`
(`{home: {name, host: {origin}, admission?, app?}}`): its bare entry opens
that workspace instead of the person's own desktop, with `app` in front (on
the workspace's desktop beside its other apps, or full tab under the app
entry; without `app`, just the desktop), and once admitted the page
replaces the address with the workspace's full link. The home is listed
first among the person's workspaces; the runtime lists no workspace of its
own.

A person invited to several workspaces, on one host or several, returns
to them from any of them. The page remembers each room it entered as a
member (an invitation redeemed, an account it holds, a device it paired;
not the home's door, and not a guest ticket) in the browser, and desktop
search offers "Open <workspace>" for the home and each of those rooms,
beside the desktop's own commands, so a person, a script and an agent
switch through the same host action. Switching saves what is pending in
the room in front, closes its desktop and its peer, then loads the other
room's link: the next room is admitted afresh, through its own door, in a
new page, so nothing of one room's state or pending work reaches the
other. Back and Forward across a switch load the room anew, never inside
the previous room's desktop. A bare desktop entry under a home reopens
the room the person last chose (choosing the home goes back to it); never
in an embed. A remembered room whose admission fails is offered as "did
not open last time" and is no longer reopened by a bare entry, and the
failure page offers the person's other workspaces. Remembering grants
nothing: every room still decides at its own door.

The `web-entry` image (`crates/browser/tilo-browser-runtime/container/`)
is the runtime's desktop bundle as static files. Its `entry.json` is
written from the environment when the container starts:
`TILO_WEB_HOME_HOST` (the workspace host's name), `TILO_WEB_HOME_NAME`
(required with a host) and `TILO_WEB_HOME_APP` (optional) make that host's
workspace the home; without them a bare visit opens the person's own
desktop. `TILO_WEB_ABOUT_NOTIFY`, `TILO_WEB_ABOUT_SOURCE` and
`TILO_WEB_ABOUT_COMMUNITY` (https URLs) become `about`, the links the
About sheet behind the top bar's Tilo mark offers; an unset one is not
offered. `TILO_WEB_EMBEDDER` (an https origin, optional) names the one
site that may frame the page as a live preview: the page sends
`frame-ancestors 'self'` plus that origin, and an embed reports its state
to it alone; unset, only the page itself frames it. The bundle lives under `v/<content hash>/` and is cached as
immutable; only the entry pages, `entry.json` and `version.json`
revalidate. The image a deploy rolls out also serves the version of the
image it replaces (`container/carry.sh`, built by `web-entry.yml` against
what production serves), so a page opened before that deploy still opens
apps it had not loaded. Only that one version is carried, never one it
carried itself: an image holds at most two, and a page survives one deploy.
A page older than that finds its version's app modules gone the first time
it opens an app it had not loaded: the loader announces the missing module,
the page reads
`version.json`, and when another version is served it offers to reload
(`web/bundle-update.mjs`). The reloaded page runs the new version only and
reopens the same windows from this browser's storage, the one that failed
included; a page never loads another version's files into its own.

`?embed=1` on an entry page is the page inside another site (the
homepage's live preview): a workspace whose host has an admission door is
watched read-only as the host's spectator person
(`POST <admission>spectate`, a device key in page memory only), so
scrolling past mints no member; the first click, tap or key press reloads
the page to join as the person. The embedding page, when it is the web
host's `embedder`, hears
`{type: 'tilo-embed', state}` (`watching`, `joined`, `full`, `unavailable`, or `unsupported` for a browser without WebGPU, which is told which browsers draw Tilo before any seat is taken).

The web app's entry serves generic static Open Graph tags; unfurlers never
see the fragment, so a preview never reveals what a link points at.

## 9. Invariants

1. Every link parses to exactly one target and every target formats to
   exactly one link per entry and mode: `format(parse(l)) == l` for canonical
   links, and non-canonical input is refused, except the slug, which is
   rewritten.
2. Nothing but the entry and the mode reaches a server: no room, record,
   resource or secret in a path or query.
3. A link never grants anything. Opening one performs the same admission and
   catalogue read as any other caller; a link to something the opener cannot
   see fails exactly like a link to something that does not exist.
4. Labels and slugs never route. Resolution uses kind, id, provider, room and
   view id only.
5. Secrets exist only in the fragment, are removed from the address bar
   before any module runs, are read once, and are redacted everywhere else.
6. Only this person's own navigations push history entries; agents,
   collaborators and remote writes never do.
7. Every resource with a declared view publishes `share-link`; no app or
   page formats a link by hand.
8. Native, browser, relay and agent code parse with the same Rust code.

## 10. Testing

- **Grammar** (`tilo-reference`): table tests for every rule in §3.2,
  including record ids with `/` (`file:` records), non-ASCII ids, both
  resource forms, app segments next to scope tokens, spans at the integer bound, and every
  refusal; property tests that `parse ∘ format` is the identity over
  generated targets and that the slug never changes the parsed target.
- **Shared corpus**: `crates/collab/reference/tests/links.json`, a list of
  `{url, target | error, secrets}` read by the Rust tests and by a
  `web/router.test.mjs` suite that runs the wasm-exported parser and the
  inline scrubber, so the one JavaScript splitter cannot drift. Both run in
  CI with the existing `web/*.test.mjs` suites.
- **Secrets**: the scrubber, run against a fake `location` and `history`,
  leaves no secret in any URL it writes; `Link::public` and `Debug` contain
  none; the formatter cannot emit one outside the fragment.
- **Router**: with a fake navigation controller, opens push, focus and
  remote changes replace, selection changes do nothing, `popstate` dispatches
  `open` for the URL, and an agent's view opening leaves history untouched.
- **Share action**: a `TestApp` check that every resource with a declared
  view publishes `share-link` with a range binding exactly when its view has
  one; invoking it through the door returns the expected URL; a host without
  a web entry refuses with its reason; `share-invite-link` is refused without
  the invite grant.
- **End to end** (headless Chrome against `tilo-relay dev`, in the style of
  `tilo-web-desktop/scripts/collab-e2e.mjs`): a member opens a chat-channel
  link in both modes and a message link scrolls to the message; two opens
  then Back and Forward; a reload restores the same place; switching mode
  keeps the target; a non-member gets the no-access page with no existence
  leak; an invitation link inspects, asks, waits, is admitted and ends on a
  URL without the secret; a bare entry with a home workspace joins it through its host's door. The existing
  `mounted_resource` agent tests switch from the version-one fragment to
  `share-link`.
- **Native**: with the recording `UriOpener` (`tilo-host`), a Tilo link for a
  member's room opens in place, a `tilo://open` link parses, and an unknown
  host falls through to the browser.
