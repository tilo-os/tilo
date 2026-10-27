# Settings, Quick Settings, and Studio ownership

Tilo uses progressive disclosure without creating “basic” and “advanced” modes. A control has one domain owner and may have several views of the same state.

## Personal and workspace settings

Tilo has two settings contexts, but not two unrelated implementations:

- **Personal Settings** is a normal application for this person's device,
  account, accessibility and fallback preferences. Its window title and
  launcher name always say *Personal Settings*.
- **Workspace Settings** is a shell-native lens opened from the active
  workspace capsule. It docks beside the live workspace instead of dimming it
  behind an app-shaped modal. Its header repeats the workspace name and local
  role, and owner/admin-only controls stay visibly disabled for other members.

Workspace Settings is contextual rather than a second launcher application:
opening a generic “Workspace Settings” icon and then asking which workspace to
edit makes high-impact permission changes too easy to misdirect. Global search
and Personal Settings may deep-link to an exact tab, but only through the
versioned `WorkspaceSettingsRoute` enum. The shell accepts the request only
when its workspace id is still the active room; it never silently switches
context before exposing governance controls.

The boundary is intentionally spatial rather than window-shaped. The active
workspace capsule gains a context mark while the lens is open, the lens enters
from that capsule, and the desktop remains visible and pointer-live beside it.
On a narrow output the same surface promotes to an inset sheet with compact tab
labels. Reduced-motion skips the travel. There is still one state model, one
mutation path, and one set of controls.

Cross-cutting domains use a single resolver. Default applications, for
example, have a personal fallback, an optional workspace-admin policy, and an
optional per-person choice within that workspace. Both settings surfaces call
the resolver; neither copies or rewrites the other scope's record.

For a resource kind `r`, resolution is explicit and deterministic:

1. A person's explicit workspace choice wins, including an explicit
   `FollowPersonal` choice.
2. Otherwise, an explicit workspace policy wins.
3. `FollowPersonal` at either workspace layer resolves through the person's
   personal default.
4. With no personal choice, the registered system recommendation is used.

Only a workspace administrator can write the workspace policy. Every person
can write only their own workspace choice and personal defaults. Application
ids must resolve through the installed-app registry and support the requested
resource type; persisted strings are never executed as commands.

## Product boundary

| Surface | Put here | Keep out |
| --- | --- | --- |
| Quick Settings | Current status and frequent, immediate, reversible actions: connect, toggle, select an already configured profile | Creation flows, destructive changes, long forms, policy editors |
| Settings | Canonical everyday configuration, setup, recovery, permissions, health, and a concise explanation of consequences | Full artifact editors, batch operations, dense diagnostics by default |
| Studio | Create, import, inspect, test, export, and deeply edit an artifact or policy | A second implementation of the ordinary status/toggle controls |

“Studio” describes a capability, not necessarily a separate window. A Studio can be opened from the launcher or as a contextual workspace launched by Settings. Both entry points host the same package and state; Settings never copies its editor.

Do not hide expert capability behind a global “advanced mode.” Prefer a stable information hierarchy with optional details, searchable actions, keyboard operation, copyable diagnostics, import/export, and a contextual **Open … Studio** action.

## Domain examples

### VPN

- Quick Settings shows connection state and connects or disconnects an existing profile.
- Settings → Network → VPN owns the whole experience: the profiles with their switches, each profile's page (its settings, export, delete), and adding one (a WireGuard tunnel from generated keys or a pasted file, a profile of an installed NetworkManager plugin's type, or a file import). There is no separate VPN app.
- NetworkManager is the runtime source of truth. Every surface calls the same typed network action boundary; none shells out independently or maintains a second connection model.

### Themes

- Settings owns the mode (light, dark, automatic), which preset each mode shows, density, your colour, and accessibility (high contrast, text size, UI scale, reduced motion, reduced transparency).
- Theme Studio owns authoring, duplication, export, contrast findings and preview of theme documents.
- The shared `appearance` record carries the canonical state (`mode`, `dark_theme`, `light_theme`, `density`, `custom_themes`); `identity.colour` carries your colour. Theme Studio writes the document of a theme it applies into `custom_themes`. Both writers merge fields they do not own.
- Every theme — preset or custom — is a `ui::theme::ThemeDocument`, the versioned contract consumed by shell chrome and every app (`docs/architecture/theming.md`).

### People and identities

- A **login account** is device scope: UID, home directory, password, avatar,
  administrator role and automatic login. AccountsService is authoritative and
  polkit authenticates every privileged change; Settings never edits passwd or
  shadow files.
- A **Tilo identity** is personal scope and is not a login account. One login may
  hold several identities. Settings shows their public fingerprints, lets the
  person select an explicit active identity, and protects active/final records
  from removal.
- The selector contains only a validated identity id. The launcher resolves it
  on the host and gives a new app one connected signing capability. Apps never
  receive a key file or the identity directory and cannot enumerate, export or
  select another key. Running apps keep the capability they started with, so an
  authored operation cannot silently change person halfway through.
- Legacy records migrate atomically to public schema-2 metadata. Private
  material prefers freedesktop Secret Service and otherwise lives in a private
  host-only broker file. Settings reports the actual backend; TPM-backed is
  shown only when a real provider sealed that identity.
- Identity creation, device enrolment, backup and key rotation remain one
  identity ceremony shared with onboarding. Settings hosts/launches that
  ceremony; it must not implement cryptography or copy secret fields into its
  collaborative settings document.

### Printing and scanning

- Settings owns everyday setup and recovery: driverless IPP discovery/addition,
  default selection, live queue health, active jobs, pause/resume, retry/cancel,
  removal confirmation and scanner discovery.
- CUPS, cups-filters and SANE are the authorities. Tilo exposes one typed narrow
  service over them and validates queue names, job ids and IPP(S) URIs before
  invoking a tool. There is no Tilo printer database or driver catalogue.
- A Print Centre is justified for long job history, document previews, advanced
  finishing, calibration and diagnostic logs. It consumes the same service and
  opens on a printer/job id; it does not duplicate discovery, state, defaults or
  queue mutation code.
- Automatic setup offers only IPP Everywhere/AirPrint/Mopria devices. Legacy
  PPD/driver installation is an explicit expert workflow with provenance and
  package review, never a background download prompted by an untrusted network
  advertisement.

## Integration contract

The target architecture has four pieces:

1. A domain service owns live state and privileged actions. UI packages receive typed status and submit typed intents.
2. One versioned persistence schema defines ownership down to the field or nested namespace. Writers perform merge updates and preserve unknown fields for forward compatibility.
3. The shell provides a versioned `ToolIntent` service. Settings can submit only a compiled enum such as `Vpn { route: Profile, profile: "Work VPN" }` or `WorkspaceSettings { route: PrivacySecurity }`. The shell maps specialist variants to known packages and handles workspace variants itself on the compositor thread. It validates workspace and context first. The wire format has no executable, package-id, argv, or permission fields.
4. A Studio applies an artifact through the same domain service. Settings and Quick Settings update from service state, not from optimistic parallel copies.

Privileged operations remain narrow and auditable: for example, the battery-cap service normalizes vendor sysfs differences, validates a supported battery and bounded threshold, and exposes only that operation through policy-controlled elevation.

## Interaction rules

- Always show the current state before the action.
- Use plain labels first; add technical identifiers in expandable details.
- Optimistic controls must roll back and explain failures.
- Hardware-absent features disappear or state why they are unavailable; never show fabricated devices.
- Destructive or session-ending actions use a focused confirmation surface with consistent motion, keyboard focus, reduced-motion behavior, and a clear default escape path.
- Privacy indicators are status, not decoration: identify the device in use and report hardware kill state separately from software permission state.
- Power-user paths must remain discoverable through search, keyboard navigation, details, logs, and export—not through duplicated controls.
