# Guided Tilo setup and migration

The product contract for the Imager's guided setup on Windows and Linux.
Platform feasibility and release validation gate each advertised configuration;
the USB-free installation path is specified in
[`architecture/usb-free-install.md`](architecture/usb-free-install.md), and
delivery status is in [`migration-status.md`](migration-status.md).

## Outcome and scope

Provide one “Set up Tilo” application for people starting on Windows or Linux.
Users choose an experience, select folders, review the result, and let setup
handle payload selection, downloading, verification, and installation preparation.
No external installation drive is required on configurations explicitly supported
by the new internal-installation path. Unsupported configurations receive an
explanation before any disk or boot changes; support is never inferred solely
from CPU architecture or the ability to mount an image.

The first release preserves the existing operating system. Full Tilo means native
dual boot. Erase-and-replace installation, a Windows-hosted Tilo desktop, macOS
migration, and direct writable links to source folders are outside this scope.

On supported Linux distributions, offer Tilo Desktop and Full Tilo. On Windows,
offer Full Tilo and explain that the desktop-only installation is available on
supported Linux distributions. Do not present an unavailable Desktop choice as
actionable. Use the repository's desktop-session compatibility contract as the
starting point, subject to release validation.

## Existing foundations and changes to the contract

Relevant existing components are `apps/imager` (host setup, release downloads,
verification, profiles, privileged helpers), `crates/session/installer` and
`os/usr/libexec/tilo-install` (target installation), and `shell/src/onboarding.rs`
(first-login setup). Existing packaging supplies Linux desktop sessions.

`docs/install-contract.json` and `docs/installer-walkthrough.md` currently describe
removable media, exclude file migration from Imager, and prohibit live-installer
filesystem resizing. The alongside path expects suitable unallocated space or
prepared partitions. These are current limitations, not delivered capabilities
of this proposal. Update the contract and walkthrough only with validated new
capabilities. Keep live-installer resizing prohibited; any future space creation
belongs to a separately validated host-side operation.

## User journey

1. **Choose an experience.** Desktop keeps the Linux installation and adds a Tilo
   session. Full Tilo installs alongside the current OS.
2. **Check this computer.** Detect OS, architecture, firmware, boot configuration,
   storage layout, encryption, and source-folder access. Identify unsupported
   conditions before downloading large payloads or making changes.
3. **Choose folders and behavior.** Offer the modes below, display source and
   destination, and calculate space. Recalculate storage recommendations when
   selections change.
4. **Allocate space for Full Tilo.** Recommend an allocation with an adjustable
   size. Include system space, selected local files, installer staging, and
   working headroom without double-counting shared staging data. Prefer existing
   unallocated space. Where space creation is not supported, explain the required
   host-side preparation and recheck afterward; do not imply automatic shrinking.
5. **Prepare automatically.** Select the correct authenticated payload, download
   with resume support, verify it, and stage required installation and migration
   state on storage that survives installation. Desktop uses packages; Full Tilo
   uses a bootable installation payload. Image format and mounting are internal
   implementation details.
6. **Review once.** Summarize the retained OS, disk allocation, folders and modes,
   download/storage requirements, and restart requirement. Present applicable
   encryption/recovery instructions before boot changes. Backend revalidation
   remains required immediately before privileged operations.
7. **Restart and install Full Tilo.** Enter a one-time internal installer without
   repeating completed choices. Target-local account and encryption secrets stay
   within their existing secure setup boundary. Review any changed facts again.
8. **Finish in Tilo.** Continue migration, open completed folders, and show pending
   folders explicitly. The desktop can open while files are still copying;
   migration is not marked complete until verification succeeds.
9. **Choose an OS at startup.** Provide an understandable boot choice. Preserve
   the existing OS as the persistent default, use an explicit one-time boot for
   Tilo onboarding, and offer “Make Tilo my default” after the user tries it.

## Folder choices

Present “Bring your files into Tilo” with three choices:

| Choice | User-facing explanation | Behavior |
| --- | --- | --- |
| Copy selected folders — recommended | Start with a copy. Your originals stay where they are; changes will not carry between systems. | One-time verified copy into Tilo-managed local storage. |
| Keep selected folders connected | Keep a local copy in Tilo. Changes carry over when synchronization runs. | Two-way synchronization when the source is safely accessible. |
| Decide later | Start with an empty workspace. Add folders anytime. | No import or synchronization is scheduled. |

Suggest Documents, Desktop, Pictures, and Downloads, plus a native custom-folder
picker. Suggestions are not consent to copy. Show sizes, access failures, and
destinations. Choose a common mode first, with per-folder overrides under
“Customize.” Deduplicate overlapping selections and explain their effective
coverage. Exclude system folders and do not silently traverse links outside the
selected scope. Source deletion is never part of a one-time copy.

“Help me choose” asks whether the user will continue working on those files in
the old OS. Trying Tilo or uncertainty recommends Copy; continued use of both
systems recommends Keep connected only where supported. Copy is also the default
for Desktop in this unified flow; opening existing local files directly remains
an ordinary file-access action, not a migration mode.

Expose “Files → Add folders” and settings for existing imports/connections after
setup. Converting a copied folder to a connection requires a comparison preview
and explicit conflict handling, never blindly replacing either side. Folder
migration grants no collaboration sharing or cloud-upload permission.

## Connected-folder contract

Connected folders are synchronized local copies, not symlinks or direct access
to original files. They consume local storage and remain usable while the source
is unavailable. Dual-boot synchronization occurs between sessions when the
source can be accessed; no background agent is assumed to run in the inactive OS.

Use a verified baseline to detect changes on both sides. Preserve both versions
when changes conflict, including edit/delete conflicts. Never resolve conflicts
solely by wall-clock timestamps. Show a simple choice with both versions kept
until resolution. Treat renames and file/directory replacements conservatively
when identity cannot be established.

Normal deletions propagate into recoverable storage with a proposed 30-day
retention period, disclosed in connection settings. If recovery space is
insufficient, pause deletion propagation rather than discard recoverable data.
Permanent user-requested deletion is a separate explicit action.

Report “Up to date,” “Changes pending,” and “Source unavailable,” with actionable
details for errors. “Up to date” requires a successful comparison with the source.
Disconnecting stops synchronization and retains both the Tilo copy and originals.

Before enabling a connection, verify read and write access, supported filesystem
semantics, source identity, and a safe mount state. Encrypted, locked, hibernated,
or otherwise unsafe sources cannot be written. Do not bypass hibernation or
encryption protections. Offer Copy only when safe read access exists; otherwise
offer Decide later. Recheck these conditions every synchronization session.
An unavailable source or permission failure must never be interpreted as deletion.

Files that are cloud placeholders, inaccessible, changing during copy, or
unrepresentable on the destination require an explicit pending/error result.
Do not claim complete migration based only on directory enumeration. Preserve
supported metadata, report material fidelity limitations, and never execute
imported content as part of migration.

## Component boundaries and data flow

Use one coordinator with focused services rather than expanding the existing
Imager view into the installation and synchronization engine:

- **Setup coordinator:** owns the reviewed plan and state transitions. The
  plan records mode, source identities, folder policies, payload identity,
  target layout, and review revision. It contains no account/encryption secrets.
- **Capability and storage probes:** return current facts and supported actions;
  never mutate storage. Windows and Linux adapters expose the same result shape.
- **Payload service:** reuses authenticated release selection, download cache,
  integrity verification, and resumable transport.
- **Platform preparation service:** executes only reviewed host-side storage and
  boot operations through a narrow privileged helper. Independently revalidates
  disk identity, layout, and available space. A changed plan invalidates approval.
- **Installer handoff:** transfers validated non-secret choices into the existing
  target installer and returns durable progress. Privileged execution never
  trusts an editable setup document as authorization.
- **Migration service:** copies selected content, records verification and
  restart checkpoints, and exposes per-folder progress without UI dependencies.
- **Connection service:** maintains two-way baselines, conflicts, recoverable
  deletions, and source-availability checks independently of installation.
- **First-login integration:** reads completed setup state, continues migration,
  and supplies later folder-management entry points.

The coordinator progresses through checking, choosing, preparing, review,
installation, first login, and migration completion. Persist explicit operation
outcomes and source/target identities across restarts. Keep credentials in
platform-appropriate secret storage or request them when needed; do not place
them in profiles, command arguments, or collaborative state.

## USB-free feasibility and recovery requirements

Mounting an ISO is not proof of bootability. Each supported platform needs a
validated boot chain, accessible installer payload, and staging outside regions
modified by installation. Firmware and encryption interactions belong in that
platform's tested support matrix. Do not advertise generic Windows/Linux support
until the corresponding configurations pass.

Preserve the old OS and its persistent default throughout preparation. A failed
one-time boot must leave a documented path back to it. Before claiming automatic
resume of disk mutations, establish an authenticated transaction record outside
the writable destination with an owner-accessible recovery mechanism. Existing
alongside installation intentionally rejects unsafe automatic reuse of partially
created partitions; preserve that behavior until a stronger recovery protocol is
specified and tested.

Downloads and file copies can resume from verified checkpoints. Disk mutation
must not be retried based solely on destination labels or a progress flag. When
safe resumption cannot be established, stop, explain the exact state, and provide
recovery instructions. Cancellation before mutation removes staged work safely;
after mutation begins, expose the operation's actual safe stop boundary.

Installer cleanup occurs only after installation and required migration data are
verified and both OS boot paths have been exercised. Never remove originals or
recovery material automatically as migration cleanup.


## Acceptance and validation

- A new user can explain Desktop versus Full Tilo, Copy versus Keep connected,
  and which OS/files remain after installation using the review screen alone.
- Windows offers Full Tilo; supported Linux offers both experiences. Unsupported
  configurations stop before storage mutation with an actionable explanation.
- On every advertised internal-install configuration, setup works without USB,
  preserves existing partitions outside reviewed changes, and boots both systems.
  Verify persistent default retention and explicit default switching.
- Test insufficient space, changed disk identity/layout, locked encryption,
  hibernation, unsupported firmware, and invalid image signatures. Refuse unsafe
  writes rather than silently falling back to a different operation.
- Inject network interruptions, process crashes, and reboots across preparation,
  installation, and file copying. Resume only authenticated/verified operations;
  retain the existing OS boot path and clearly report recovery requirements.
- Exercise Copy, Keep connected, and Decide later, including per-folder overrides,
  overlapping selections, links, unusual names, large files, unavailable sources,
  cloud placeholders, and metadata limitations. Verify originals are preserved.
- Exercise offline edits on both sides, edit/edit and edit/delete conflicts,
  recoverable deletions, recovery-storage exhaustion, reconnecting, converting a
  copy to a connection, and disconnecting. No silent loss is acceptable.
- Validate privileged-helper boundaries and plan tampering; secrets must not
  appear in profiles, logs, process arguments, or collaboration state.
- Use VM fault-injection tests plus representative physical hardware. Existing
  installer tests remain required; documentation-only approval is not evidence
  that internal boot, resizing, or synchronization already works.

## Technical references

- Repository: `docs/install-contract.json`, `docs/installer-walkthrough.md`, and
  `apps/imager/packaging/README.md`.
- [GNU GRUB: loopback booting](https://www.gnu.org/software/grub/manual/grub/html_node/Loopback-booting.html)
  describes why reading an image is distinct from the OS supporting image boot.
