# System settings service boundaries

Settings is a client of system authorities; it is not a second implementation
of them.

## Reaching the authorities from the sandbox

Settings runs in the ordinary app sandbox, which has neither the host's client
binaries nor the system bus. The launcher's `system_authority` grant gives it
both, and only it: the image names the receiving package ids, and no manifest
can claim the grant. Settings gets `/usr/bin`, `/usr/sbin`, `/usr/libexec` and
`/etc` read-only. It reaches the system bus through its own `xdg-dbus-proxy`,
filtered to the named authorities (NetworkManager, BlueZ, logind, systemd,
timedated, localed, hostnamed, AccountsService, UPower, power-profiles,
fwupd, firewalld). It also gets the PipeWire socket and the compositor's
runtime state files (connected displays and input devices), with every
subdirectory of that state masked. The print spooler's socket stays out, as
it does for every sandbox: printing goes through the Print portal. Each authority
still decides what the person may change. When a read fails, the pane shows
the reason as an error. It never shows a placeholder as if it were the
device's state.

## System information

An app sandbox has a process namespace of its own: its `/proc` shows the app
and nothing else, and binding the host's would hand every program's command
line and environment to it. Machine readings and the process list are a
typed service of the session instead (`tilo-system-stats`), reached like the
other session services: the compositor serves
`<services root>/system-stats.sock`, which no sandbox can reach, and the
launcher opens a connection for an app that declares the `system-info`
capability, writes the app's id, room and grants as its first frame, and
hands it to the app (`TILO_SVC_SYSTEM-STATS_FD`) before the sandbox starts.

- **What is sent.** Processor (total and per core), load, memory and swap,
  pressure (PSI), disks (space and I/O), network throughput, graphics load
  and memory where the driver reports them, batteries and temperatures; the
  running apps, each its processes taken together by the app's cgroup; and a
  process list with pid, parent, name, app, user, processor, memory, threads
  and state. A process's command line is sent only with its own grant. Its
  environment, open files and memory never are: the service does not read
  them.
- **Pushed, not polled.** An app subscribes at an interval and is sent a
  snapshot each interval. One thread reads for every subscriber, and only
  while there is one; nobody subscribed costs no thread and no read.
- **Grants.** `system-info` covers the readings; `processes` covers the app
  and process lists and ending the person's own; `process-commands` covers
  command lines. The launcher grants an app the image ships what it
  declares of the first two; any other app gets what the room's capabilities
  name (`Scope::Device`) and asks for the rest through the ordinary
  permission prompt, which the service raises and whose answer it keeps as
  the portal keeps its choices. The manifest is the ceiling of every grant.
  `processes` alone keeps its older meaning (starting programs of its own)
  and opens no connection.
- **Ending.** "End app" and "End process" are Monitor's actions, confirmed
  like every destructive action. The service ends only processes that are
  the person's and live in their own `app.slice`, never a system service or
  the login session that draws the screen. A request names the process's
  start time too and the process is pinned with a pidfd before it is checked,
  so a reused pid is never signalled.
- **The desk.** The System monitor widget reads the same hub in the
  compositor, subscribed only while it shows.

## Ownership and roaming

Every leaf in the shared `settings.json` contract has an explicit entry in
`settings_store::continuity`. The registry is the boundary, not a section-name
convention. A new leaf that has not been reviewed is readable by its owner but
is excluded from personal-device continuity.

The scopes are deliberately distinct:

| Scope | Owner and transport |
| --- | --- |
| Personal | Converges only between devices certified to the same identity. |
| Workspace | Lives in the workspace record plane and follows its collaborators and permissions, never the personal settings file. |
| Device-local | Remains on the machine whose hardware or local environment gives it meaning. |
| Secret | Uses a purpose-built sealed transport or local credential vault; never ordinary settings sync. |
| Derived | Recomputed/runtime history and never treated as durable personal intent. |

Examples matter more than the section name. Theme, locale, accessibility,
shortcuts, text assistance, and Quick Settings arrangement are personal.
Connector layout/scale, audio node selections, tablet mapping, calibration,
power policy, update channel, portal-granted paths, loopback model endpoints,
and Continuity privacy toggles are device-local. Workspace roles, default-app
policy, and permissions remain workspace state. API keys, local credentials,
Wi-Fi passwords, and NetworkManager secrets are secret.

The device fabric projects settings through the typed registry and resolves a
key by `(counter, device identity)`, giving the same winner regardless of
arrival order. The shell writes a bounded mode-`0600`
`continuity-provenance.json` projection for Settings. It contains canonical
keys, certified device labels/ids and revisions. Wi-Fi rows contain only keyed
network id, SSID, security kind, and origin. Ciphertext envelopes and plaintext
passwords are not representable in the projection.

Settings presents the winner and any devices still advertising a different
value. This is explanatory UI, not a second merge engine. Editing the ordinary
setting creates a new local revision through the existing owner. A malformed
or future-version provenance file is refused rather than partially displayed.
Version 1 provenance (settings origins only) migrates on read to version 2;
version 2 adds conflict alternatives and sealed Wi-Fi profile provenance.

## Updates

- `bootc` owns image deployments and rollback targets. The update pane starts
  `tilo-deployment-update.service`, then reads `bootc status --format json`.
- `fwupd` owns firmware discovery, signature verification and installation.
  OS and firmware state are presented together, but their transactions remain
  independent.
- systemd owns update timing. The narrowly authorized
  `tilo-update-policy@.service` accepts five fixed instances and writes only the
  bootc timer override and `/var/lib/tilo/update-policy`.
- `tilo-update-policy-check` is an `ExecCondition` on the bootc service. It asks
  NetworkManager whether an active link is explicitly metered. Unknown state
  does not postpone security updates; an explicit metered link does when the
  policy is `avoid`.
- history comes from the bootc service journal. Release-note links are accepted
  only from HTTPS OCI image metadata. Rollback health reports the retained
  deployment and Tilo boot-confirmation timer independently.

## Region and time

- timedated is the authority for time zone and NTP enablement. Zone choices
  come from `timedatectl list-timezones`, i.e. installed IANA tzdata.
- the searchable selector and compact coordinate map use that same accepted
  identifier set. Map coordinates are joined from the installed tzdata
  `zone1970.tab`/`zone.tab`; there is no second embedded city or coordinate
  database. Canonical zones with published coordinates appear on the map,
  while aliases and coordinate-less identifiers remain available in the
  selector. The UI never implies that a point is device geolocation.
- number/date/time/measurement/first-weekday choices live in the shared locale
  section. Keyboard input sources remain owned by Mouse & Keyboard; Region does
  not duplicate them. See [Input sources](input-sources.md) for the per-user
  XKB ownership and migration contract.

## External accounts

- connection metadata is in the canonical `accounts::AccountStore`; secrets
  remain in `SecretVault`.
- operational health has a separate atomic `sync-status.json`. Account
  consumers report sanitized state transitions; Settings only reads them.
  Mail currently reports connecting, successful fetches, OAuth completion and
  authentication/error outcomes. A missing report is displayed as “Not checked
  yet”, never “Connected”.
- Settings does not expose Nextcloud service switches until Mail, Calendar,
  and Files consume the same canonical fields. Protocol-specific authorization
  remains with its owning app rather than being reimplemented in Settings.
- removal is confirmed and explains local versus provider data. Account
  metadata, sealed credentials and runtime health are removed together.

Runtime consumers report health through `accounts::AccountSyncReporter`. It is
the lifecycle writer for `started`, `succeeded`, `offline`, authentication
required, and bounded generic failures, and it preserves the previous
successful-sync timestamp. Consumers keep protocol errors in private logs and
never persist URLs, credentials, response bodies, or raw transport errors as UI
status. Calendar uses it around the background CalDAV pass. Files status
must be emitted by the eventual WebDAV/Nextcloud network worker, not inferred
from Files' read-only hydration badges; this repository does not yet contain
that worker.

Printer settings consume `os_actions::printing` rather than maintaining a
printer inventory. The service asks local CUPS for standard IPP printer/job
attributes through `ipptool`, overlays the result on `lpstat`'s queue model, and
bounds all device-provided names/reasons before returning them. Missing marker
or impression attributes remain `None`; a failed query is distinct from a
printer which answered but omitted supplies. Settings says "not reported" or
"unavailable" instead of estimating ink or page progress.

## Notifications and accessibility

- per-app notification rules are keyed only by installed/previously-configured
  package IDs. The compositor applies them using launcher-attributed callers,
  never the sender-controlled freedesktop `app_name`.
- priority is a user decision; temporary mutes are absolute deadlines. Priority
  contacts come from a launcher-verified Contacts projection containing only
  salted endpoint digests. Communication apps require a separate per-app grant
  before a matching contact may pierce focus mode; the authorized app remains
  trusted for its sender assertion until transports are OS-owned.
- accessibility controls are writable only when a compositor or service applies
  them. The compositor owns grayscale/invert output filters, the pointer-centred
  magnifier lens, pointer guides, large cursor, sticky/slow keys and configurable
  two-switch scanning. The shared toolkit owns exact wider text carets. Apps own
  semantic narration and apply the selected speech rate/voice through Speech
  Dispatcher or eSpeak NG; shell chrome owns visual alerts; the signed OSK owns
  its private input-method client. Settings is policy UI for all of these, not a
  second runtime.
- the magnifier is deliberately labelled a **lens**. Its centre follows the real
  hardware pointer and the unscaled scene remains the hit-test authority, so the
  item at the lens centre is exactly the item clicked. Full-screen zoom is not
  offered until every client/shell coordinate and hardware cursor can share an
  inverse transform.
- Vello's final-scene blend modes provide exact W3C grayscale and white-difference
  inversion, but no general colour matrix. Protan/deutan/tritan filters therefore
  remain absent rather than shipping approximate or misleading transforms.
- live captions remain a read-only unavailable capability until the image ships a
  local speech-to-text engine and model lifecycle. No caption preference is
  persisted today. Arbitrary Wayland-client caret/focus highlighting likewise
  awaits a trusted geometry protocol; Tilo toolkit text carets already work.
