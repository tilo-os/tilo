# Installer and migration model

[Installation experience](installer-experience.md) is the product contract.
This page describes how the code meets it: one guided flow, and one way to move
a kind of data along a route. Every data kind and every route stays. What goes
is the per-combination code (custody, journal, self-check), which today is
written separately for most kind × route pairs. Code that is not yet on this
model moves onto it one kind or route at a time, and the code it replaces is
deleted in the same change.

## One guided flow

There is one installer UI: the guided setup, launched as `tilo-imager --setup`.
Its pages follow the contract's screen order:

1. Destination: this computer (Tilo OS), another computer, or Tilo Desktop.
2. This computer, or the target computer's compatibility.
3. Account and computer: new identity, or join an existing one; computer name.
4. Your data: the source-oriented selection.
5. Space for Tilo.
6. Review.
7. Install or Ready: download, verify and stage; or prepare the medium.

The host also has the image writer and the later folder imports
(`--import-folders`). Those are steps that the flow opens, not separate apps.

A route is offered when the host, the build and the release catalog can fulfil
it. Otherwise the page shows it as unavailable, with the reason. Gating is a
capability check on one flow, never a second UI. A gate stays shut until its
qualification passes (for example the USB-free install's
`TILO_IMAGER_INTERNAL_INSTALL`).

The flow runs in three places, with the same pages, actions and operation
payload:

| Place | Binary | Pages it adds or skips |
|---|---|---|
| Host, before install | `tilo-imager --setup` | Writes media, stages the USB-free install |
| Destination, on the live medium | `tilo-installer` | Adds keyboard and network; disk and review answer only the questions the host left open |
| First boot | `tilo-firstboot-*` | Login password and outstanding connections only |

The destination is not a separate wizard. It consumes the operation the host
produced, using the setup's own pages. Its console and unattended recipe drive
the same published actions as a click does.

## Data kinds × routes

### Kinds

What is moved. `portable_transfer::Purpose` is the one kind enum.

| Kind | Source | Applied by |
|---|---|---|
| Home | Granted folders with exclusions, including cloud and Steam trees | The verified copier (`migration.rs`), or the live retained projection |
| Applications | Flatpak apps and their data | Flatpak restore |
| Browser | Products and profiles: bookmarks, history, supported sessions | Canonical signed first-launch import |
| Accounts | Mail (Thunderbird), contacts, calendars, GNOME Online Accounts, iCloud | The account store and the streaming mail importer |
| Service sessions | Declared sessions of supported services | Sealed account custody |
| Identity | New identity, or a device certificate from an existing one | Enrolment on the destination |

Windows and Linux are source platforms, not kinds. They differ only in how a
kind is read.

### Routes

How the data reaches the destination.

| Route | Carrier | When it is applied |
|---|---|---|
| Same computer | The retained source disk, after the reboot handoff | First boot copies it, or projects Home live (retained Home) |
| Prepared medium | Sealed units captured onto the installer medium | Installation or first boot on the destination |
| Portable | One password-sealed archive file | Whenever it is opened on a Tilo device |
| Other device | A paired Tilo device delivers the package (`tilo_handoff`) | After approval on the destination |
| Desktop | Same OS, same disk; nothing is asked about data before installing | At the first Tilo login, which offers the familiar folders for copying |
| Later imports | Folders chosen after install | Immediately |

### One pipeline

Every (kind, route) pair goes through the same five stages. Only the kind's
reader and applier, and the route's carrier, are specific:

1. **Select.** The kind's source inventory produces a reviewed selection with a
   revision. Review fails if the source changed since.
2. **Seal.** The selection becomes sealed units in the one unit format: the
   `portable_transfer` frames over `tilo_seal`, tagged with the kind. Every
   route carries the same units.
3. **Carry.** The route moves the units: retained disk, medium partition, file
   or peer stream.
4. **Admit.** One expectation `{operation, kind, route, digest, size, setup
   binding}` per unit, kept in one manifest keyed by kind. The destination
   admits a unit only if it matches its expectation and the image's declared
   ability to apply that kind.
5. **Apply.** The kind's applier writes into Tilo.

### One journal

Crash recovery has one shape:

- **Operation record.** Phases `Prepared → Sealed → Carried → Admitted →
  Applied → Verified`, or `Failed` or `Cancelled`. It is published owner-only
  through `StorageDirectory`.
- **Copy log.** The framed, checksummed, append-only `migration_journal_log`.
  Its header names the operation, kind, route, source identity and
  destination. Records are kind-specific receipts.

On reopen, the source identity and the destination bytes are revalidated. A
receipt is a hint to revalidate, never authority to overwrite. Old record shapes
are refused, never upgraded.

### One custody and one self-check

- **Custody.** Privileged work goes through one helper protocol whose requests
  are pipeline stages (capture a selection, admit a unit, apply a kind). There
  is one helper mode per stage, not per kind × route.
- **Self-check.** The signed image declares which kinds and protocol versions it
  can apply. Admission compares each unit's expectation with that declaration.
  There is one capture protocol version. Qualification runs this same path in
  VM and native tests. It is not a second production path.

## Tests

The tests that pin behaviour stay and move with the code they cover:

- the live journey through production effects (golden path, main story, other
  device, your-data integration, restart and handoff);
- the installer's headless wizard, unattended recipe and console surface;
- the copier's journal recovery;
- the VM install cycles and the native journey fixtures.

Tests of layout, widget structure and private custody internals go with the
code they pin.
