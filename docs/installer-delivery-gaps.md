# Installer delivery status

## Readiness

**The implementation is not yet a qualified production distribution.** The
product contract is [Installation experience](installer-experience.md). This page
distinguishes implemented paths from the remaining functional limits and release
evidence. Passing a fixture, a TPM simulator or a protocol test is not a physical
installation claim.

The older [migration status](migration-status.md) is historical, particularly for
Windows USB-free staging, which is existing functionality and remains supported
by its native path.

## Implemented paths

| Area | Implementation | Evidence and remaining qualification |
| --- | --- | --- |
| Screen order | This-computer compatibility precedes personal setup. Purpose is absent from new journeys; old optional purpose payloads remain readable. New identities prepare asynchronously from Profile without a preparation-password screen. | Live production-effect tests reach Review on this- and another-computer routes, including asynchronous identity creation. Native disk mutation is not performed by these tests. |
| Your data | Source-oriented browser/profile, account, Thunderbird folder, Steam and file trees; mixed-state toggle-all/source/group controls; independent disclosure; atomic selection rollback. Folder exclusions reach the actual Home capture. | Production-widget/action tests and real-source selection tests. Refresh uses opaque root/entry identities instead of sibling indices; private names are not published in status metadata. |
| Browsers | Distinct installed products/profiles, real saved-login metadata counts, per-profile bookmark/history choices and associated supported session choices. Deferred bookmarks/history use canonical signed first-launch import. | Selection, source ownership and signed completion tests pass. General saved-password, extension/settings and arbitrary website-session transfer are not complete; unsupported/reconnect states remain explicit. |
| Thunderbird | Accounts, folders, local mail, flags, attachments, address books and calendars. Same-computer first boot streams retained mail rather than collecting a whole mailbox in one encrypted package. Canonical Mail cache merge preserves existing state; Contacts/Calendar consume standard-format import inboxes. | Real mail/cache round trips, selected-folder exclusion, unchanged-source, retry, attachment-conflict, sidecar import and a mailbox larger than 64 MiB are tested. Large cross-computer mail remains bounded by the prepared-package carrier below. |
| GNOME Online Accounts | Selected provider identities and enabled services travel in an authenticated private source journal. First boot creates/reuses canonical destination service accounts; unavailable credentials become individual reconnect states. | Source revalidation, selected-service handling and GOA/Thunderbird deduplication tests. No root adoption of the source credential vault. Service usability still needs installed-system acceptance. |
| Files, cloud and Steam | Granted-source trees, actual local-byte accounting, explicit placeholder/gap states, redirected/overlapping-root handling, per-source exclusions, library/userdata/Proton metadata and retained/copy integration. | Classifier, identity, selection and source-preservation tests. This is not proof of complete game-specific save coverage or working cloud hydration/synchronization. |
| Same-computer protected handoff | A generated high-entropy preparation secret protects the existing canonical package. When a verified target policy is available, PCR 11 signed-policy authorization plus PCR 7 binds the secret to the actual TPM and target generation. Large mail/files/apps remain in the authenticated retained-source journal; no duplicate account/browser import is scheduled. | Real `swtpm` seal/unseal, wrong-TPM, PCR, authority, ownership, package-binding and composite large-mail tests. Physical firmware/TPM and full reboot acceptance remain outstanding. |
| Another computer | Specific model/report/architecture selection; cross-architecture catalog selection without host fallback; private resumable target choices. Source peer handoff uses canonical device certification, explicit code approval and receipt-bound package delivery. Fresh destinations can join the package's identity; unrelated identities are refused, not overwritten. | Independent runtime-peer tests include fresh identity bootstrap, selected-mail import, wrong operation/certificate/approval and exact signed receipt. Physical media boot and native Home/apps receiving qualification remain outstanding. |
| Unattended target installer | Private FD or no-follow private-file recipe, optional canonical account-password hash, explicit disk intent, real TPM probe and target preflight. Uses the existing published actions. Failure retains a working canonical console endpoint, including a private fallback endpoint when needed. | Installer unit/action/headless tests. Currently an explicit whole-disk erase recipe, not unattended alongside allocation or data migration. Normal interactive alongside behavior is retained. |
| Desktop only | Existing direct package/session route, without OS-account creation, migration, naming or partition questions. Existing user/files/apps remain. | Route and compilation tests; a clean-host packaged installation and first-session switch still need release acceptance. |
| Packaging | Pairing helper included in tar/AppImage/Flatpak/platform archives; native prepared/extraction helpers retained. Target-policy catalog/download validation added. | Packaging and catalog tests. A development checkout is not an installable released distribution. |

## Release blockers

1. **Publish actual matching payloads.** The checked-in
   `apps/imager/release-catalog.json` is empty. OS, Desktop and target-policy
   payloads must be authenticated, version-compatible and included by the real
   release pipeline. Do not use a synthetic catalog as release evidence.
2. **Perform a current-source installed-system round trip.** Install from the
   actual package into an owned disk-backed VM, cold-boot Tilo, exercise imported
   data and the retained OS, and check the persistent startup default. Repeat on
   representative physical machines. Historical receipts do not qualify changed
   code.
3. **Run native Windows acceptance.** Keep existing USB-free ARM64/x86 staging.
   Cross-builds and Linux peer tests do not prove Windows shrinking, encryption,
   Fast Startup handling, source capture, firmware boot or destination import.
   The deferred native Windows workflow remains disabled until genuinely usable.
4. **Close current regression and policy results.** Keep exact-source test and
   lint results; distinguish concrete environment restrictions from regressions
   by checking the actual base. Do not ignore failing tests, raise portability
   allowances, suppress warnings or disable production paths to obtain green.
5. **Validate every advertised package on a clean host.** No checkout, manually
   supplied helper or development environment may be necessary. Exercise runtime
   acquisition, pairing, policy acquisition, elevation and first boot.

## Remaining functional limits and suggested completion

### Browser credentials and website coverage

Recognized websites and saved-login metadata are not proof of transferable
sessions. Complete the capability matrix for each browser/profile and each
selected data kind. Extend only through canonical source/receiving grants and
native credential access; never copy an arbitrary credential vault as root.
Verify actual destination sessions and reconnect behavior, including stale and
expired sessions. Bookmarks/history import must not be presented as complete
browser migration.

### Large cross-computer mail

The legacy encrypted prepared package remains bounded to 32 MiB for the whole
package; the selected-mail encrypted bundle is also bounded and materialized in
memory. Same-computer mail avoids those limits through the retained-source
streaming producer. For large cross-computer mail, add a separately authenticated
chunked carrier using the existing portable-transfer codec, with declared size,
digest and reviewed selection. Authenticate it in media extraction and receiving
custody, then feed the same streaming Mail importer. Do not increase the whole
package bound or build a multi-gigabyte JSON vector.

The streamed importer currently budgets 8 GiB of source mail and 1,000,000
messages, with a 16 MiB individual-message bound and 32 MiB sidecar bound. The
Mail kept cache remains the app's bounded 256 MiB per-account JSON format.
Oversized or changed data is refused/reported, not silently trimmed. Extend the
shared Mail storage/streaming representation for cases beyond these bounds.

### Identity and protection fallback

A preparation machine cannot attest another machine's protection from its model
name. Another-computer key release requires a real certified receiving device
and explicit source approval; the source must remain running. There is no
persistent source daemon/restart recovery for that new peer session yet. Keep
its expiry and keep-on requirement explicit, and preserve the legacy encrypted
offline route.

On the same computer, automatic protected handoff requires a signed target policy
bundle. Without one, the retained-source journal handles supported data and
reconnects; complete the explicit existing-identity first-boot approval model
rather than silently creating a different identity or claiming key continuity.
User adoption remains fixed at UID 1000 until authenticated user mapping exists.

### Steam, cloud files and source completeness

Validate Linux/Windows libraries, external drives, non-cloud saves, Proton prefixes
and game-specific locations against realistic source machines, then load games
on the installed system. A Steam Cloud flag is not uploaded durability.

Reconcile OneDrive redirected folders with local storage, cloud placeholders and
other selected roots. Offer hydration or a destination connection only when the
actual provider supports it; disclose download/space consequences. Verify the
local and remote portions independently. Connected folders remain unavailable
until the synchronization/conflict/deletion-recovery service is complete.

Users can test and report missed sources; the installer must make those sources
recognizable and its gaps visible. Test discovered data against the installed
result rather than treating export completion as successful migration.

### Hardware quality and evidence

The component report contains typed observations, applicability, reasons and
remedies. A driver working on the preparation OS is not evidence that it works
in the selected Tilo release. Imported/model reports are display facts, not
installation authority; the actual destination is rechecked.

Still needed:

- Applicable model/component evidence tied to the target kernel, firmware,
  drivers and release, including specific ARM variants.
- Physical hybrid Intel/AMD integrated plus NVIDIA/AMD discrete graphics:
  acceleration, offload, external outputs/docks, suspend/resume and idle power.
- Ordinary laptop display/brightness, lid, battery, networking, Bluetooth,
  camera, microphone, headset and fingerprint behavior.
- Applicable firmware/routing and measured/listened-to model-specific speaker
  tuning. Device enumeration alone is not good sound.
- Physical fractional-scaling text quality and complete assistive/keyboard
  traversal on realistic large source inventories.

### Unattended and native receiving acceptance

Extend unattended recipes to explicit alongside allocations and selected data
only through the same reviewed plan and helper revalidation. Preserve the
interactive console on any mismatch. Exercise TPM absence, failed unlock,
changed disks, interruption and recovery-key delivery on real target boots.

Qualify Home/apps receiving on a real owned GPT/native-store subject for both
media and peer delivery. A device-signed package-import receipt names that exact
stage; it is not a substitute for native filesystem closure or proof that every
background migration operation finished. Never remove originals on that basis.

## Target-policy producer contract

A target policy bundle contains `tilo-target.efi` and exactly one signed
`predictions/generation-<N>.json` plus its `.uki.sig`. The helper embeds the
release authority; a caller cannot supply a new trust anchor. On a source host,
the catalog's `tilo-target-policy` entry is downloaded and validated before the
helper reads it through held no-follow original-caller-owned descriptors. A
live medium may carry the corresponding bundle under `/boot/tilo-target`.
Generation, target UKI, package digest, receiver UID, TPM identity and sidecar
binding must all agree. An absent policy artifact is not permission to invent an
unbound TPM blob.
