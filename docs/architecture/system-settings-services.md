# System settings service boundaries

Settings is a client of system authorities; it is not a second implementation
of them.

## Updates

- `bootc` owns image deployments and rollback targets. The update pane starts
  `qos-deployment-update.service`, then reads `bootc status --format json`.
- `fwupd` owns firmware discovery, signature verification and installation.
  OS and firmware state are presented together, but their transactions remain
  independent.
- systemd owns update timing. The narrowly authorized
  `qos-update-policy@.service` accepts five fixed instances and writes only the
  bootc timer override and `/var/lib/qos/update-policy`.
- `qos-update-policy-check` is an `ExecCondition` on the bootc service. It asks
  NetworkManager whether an active link is explicitly metered. Unknown state
  does not postpone security updates; an explicit metered link does when the
  policy is `avoid`.
- history comes from the bootc service journal. Release-note links are accepted
  only from HTTPS OCI image metadata. Rollback health reports the retained
  deployment and qOS boot-confirmation timer independently.

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
status. Calendar uses it around the real background CalDAV pass. Files status
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
  awaits a trusted geometry protocol; qOS toolkit text carets already work.
