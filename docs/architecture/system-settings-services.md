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
- the selector and future visual map share `TimeZonePoint`; there is no second
  embedded time-zone list.
- number/date/time/measurement/first-weekday choices live in the shared locale
  section. Keyboard input sources remain owned by Mouse & Keyboard; Region does
  not duplicate them.

## External accounts

- connection metadata is in the canonical `accounts::AccountStore`; secrets
  remain in `SecretVault`.
- operational health has a separate atomic `sync-status.json`. Account
  consumers report sanitized state transitions; Settings only reads them.
  Mail currently reports connecting, successful fetches, OAuth completion and
  authentication/error outcomes. A missing report is displayed as “Not checked
  yet”, never “Connected”.
- Nextcloud's existing `use_mail`, `use_calendar` and `use_files` fields are the
  granular service controls consumed by apps. Fixed-purpose IMAP, CalDAV and
  WebDAV accounts do not get meaningless duplicate switches.
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
  contacts remain unavailable until there is a verified Contacts identity
  source.
- accessibility controls are writable only when a compositor or service applies
  them. High contrast, reduced motion, text size, large cursor, sticky keys and
  slow keys have consumers. Colour filters, magnification, caret aids, switch
  control, captions, visual alerts, screen reader and OSK are capability rows
  until their secure end-to-end paths exist.
