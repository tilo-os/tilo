# Installation experience

## Contract and scope

This is the product specification for installing Tilo OS on the current computer,
preparing it for another computer, and adding Tilo Desktop to an existing Linux
installation. It specifies the intended experience, not delivered capabilities.
[Delivery gaps](installer-delivery-gaps.md) records implementation and evidence
still needed. Where older walkthroughs differ on screen order, terminology or
Desktop migration, this document takes precedence for the intended experience.
Existing security and installation safety contracts still apply.

The installer does the technical work. The person chooses the destination,
identity, data and storage allocation, then approves the concrete result.
It does not ask the person to choose image formats, a server/computer purpose,
bootloader machinery or migration implementation details.

The initial OS installation preserves the existing OS and its persistent startup
default. Erasing the existing system is not an implicit fallback. An unavailable
installation method must not silently become a different operation.

## Common interaction rules

- Standard name, account and computer-name controls remain familiar. Optional
  profile pictures are optional. Do not invent additional identity questions.
- Use the heading **Your data**, not “Belongings.” Organize data by recognizable
  source applications, accounts and storage locations, not importer subsystems.
- Defaults do useful work. Select eligible data by default after discovery within
  authorized scope. Clearly disclose this selection before transfer. Selection
  never grants cloud-upload, collaboration-sharing or deletion permission.
- One toggle-all, mixed-state source toggles and individually selectable children
  use the shared controls. Disclosure and selection are separate actions.
- Back returns to the actual prior screen or closes the current disclosure.
  Choices, field editing state and scroll position survive navigation. A review
  edit returns to review, not the beginning of the wizard.
- Errors appear beside the relevant control when it has been used; invalid
  submission reveals the field. Correcting a field clears its error. Do not make
  people submit repeatedly to discover one invalid field at a time.
- Navigation stays reachable at small sizes and accessibility scaling. Overflow
  has a visible affordance. Typing, selection and scrolling do not recreate an
  entire source inventory or move unrelated controls.
- Use real information. An illustration is justified when it explains a real
  relationship or operation, not when it fabricates a fleet. An identity-approval
  diagram can show this computer and the actual approving device; its geometry
  must remain stable while the computer name changes.
- All decisions and operations have semantic actions and accessible state,
  including mixed selections, compatibility details and errors. Pointer input
  is not the only way through setup.

## OS on this computer

### 1. Destination

Choose **On this computer**, then **Tilo OS**. On supported Linux distributions,
Tilo Desktop is a distinct alternative. Do not offer Desktop on Windows as an
available operation.

### 2. This computer

Start an unprivileged hardware and installation-feasibility scan automatically.
Show the detected model and important component results. Scan facts are not
permission to install; the privileged operation independently revalidates them.

The report covers graphics, internal and external displays, Wi-Fi, Ethernet,
Bluetooth, audio, microphone, camera, input devices, fingerprint reader,
suspend, battery, storage, firmware and boot configuration. Distinguish:

- Supported by applicable evidence.
- Supported with a named limitation.
- Unsupported.
- Not verified.

Expand important limitations; keep working details compact. For example, an
unsupported fingerprint reader means password sign-in, not an unexplained failure
of the entire computer. A blocking issue has a specific remedy and **Check again**.
Check actual release availability and installation space before collecting
identity or migration work. Do not pass a generic architecture check off as a
model-level compatibility report.

### 3. Account and computer

Use an existing Tilo identity, restore a backup, or create an identity. A new
identity asks for a name and optional picture. Existing-device approval shows
only authenticated information about the participating devices. Confirm or edit
the suggested computer name. Do not add a Purpose screen.

Normal OS login-password setup happens on the destination during first boot.
There is no mandatory separate 15-character preparation-password screen.
Hardware-backed protection is used automatically where the actual destination
supports the required protection. Do not explain TPM terminology in the normal
flow, and do not confuse hardware-backed protection with a login password.

Removing that screen does not permit plaintext export of identity keys, login
sessions or credentials. The implementation must provide a protected handoff or
leave the sensitive connection for first boot. See the security requirements
below and the delivery gaps for the unresolved handoff work.

### 4. Your data

Discover actual sources, apply the source-selection contract below, and explain
what will be copied, connected, reinstalled or require signing in again.
Discovery is not consent to transfer until the reviewed plan is approved.

### 5. Space for Tilo

Show the actual drive with a before/after allocation. Recommend system space,
selected data, staging and useful working headroom without double-counting
shared content. Update the recommendation as data selection changes.

Allow an understandable size adjustment, not manual partition design. Prefer
existing unallocated space. Where supported shrinking is needed, name the
partition and its before/after size. Where it is not supported, explain the
required preparation rather than implying automatic resizing.

### 6. Review

Show one editable summary, using real values, that answers:

- Which computer and drive receive Tilo?
- Which existing OS and files stay unchanged?
- How much space does Tilo get, and which partition changes?
- Which data comes from where, and what is excluded or incomplete?
- Which accounts need signing in again?
- What must be downloaded, and when will the computer restart?
- Which OS starts by default afterward?

Example wording, with illustrative values only:

> Install Tilo alongside Windows on this computer. Windows and its files stay.
> Tilo gets 120 GB on the internal drive. Copy 38 GB from Documents and Pictures.
> Add two mail accounts; one requires signing in again. Download 8 GB.
> Installation requires a restart. Windows remains the default startup choice.

Do not repeat a long explanation of every previous screen. Material limitations
must be visible without opening a technical log. Present any required existing-OS
recovery-key instructions before changing boot configuration.

### 7. Install

**Install Tilo** approves the reviewed operation. Download, verification and
staging run without image-management questions. Display useful progress and
actual blockers. Request elevation only when needed.

Before reboot, offer **Restart and install**, with a save-work reminder. Do not
restart unexpectedly. The destination installer consumes completed choices and
asks again only when facts materially changed or a required choice is unresolved.

### 8. First boot

Set the local login password and finish genuinely outstanding connections.
Do not repeat completed identity, computer-name, data or disk choices. Open the
desktop once usable; show specific outstanding data operations separately.
Offer making Tilo the default OS after it has been tried, not by silently
changing the retained OS default.

## OS on another computer

Screen order:

1. **Destination:** choose another computer.
2. **Destination compatibility:** identify manufacturer, model and relevant
   variant, or obtain a hardware report from that computer. Model evidence and
   an actual scan are labeled differently. Optional hardware, disk feasibility
   and protection capability require checking the real destination.
3. **Account and computer:** choose identity and destination computer name.
   Interactive installation defers its login password to first boot.
4. **Your data:** explicitly establish the source: this computer, another
   authorized Tilo device or supported backup, the destination computer, or no
   data. Never assume the preparation computer's data belongs on the destination.
5. **Installation method:** offer methods actually supported for that situation.
   For USB, identify the removable drive by name and size and disclose its erasure.
   Keep it distinct from the destination's internal drive. USB-free installation
   requires a genuine way to prepare that destination, not just a model match.
6. **Review and prepare:** summarize destination, compatibility limitations,
   source data, media erasure and choices deferred to the destination. Download,
   verify and prepare the medium or supported alternative.
7. **Ready:** provide concise destination-specific boot instructions. Do not
   claim a universal firmware key for machines whose key is unknown.
8. **On the destination:** verify actual hardware, obtain destination-local data
   choices where requested, choose space, and confirm the resulting operation.
   Do not repeat completed choices. Finish the login password at first boot.

### Unattended installation

This is an explicit mode, not the normal interactive flow. Before proceeding it
requires the account credentials, a concrete disk plan, data-source access and
an actual destination security check. A model lookup cannot attest hardware
protection or authorize a disk write.

Protect credentials in transit and at rest; never place them in public media
metadata, logs, action responses or command arguments. The absence of suitable
hardware protection needs an explicit secure alternative, not a silently weakened
plan. If destination facts differ, refuse the conflicting operation and offer
interactive completion with a precise error. Do not guess a disk or hang at an
invisible prompt. Console and semantic control must expose the same recoverable
state without exposing secrets.

## Tilo Desktop only

Screen order:

1. **Destination:** this computer, Tilo Desktop.
2. **Compatibility and review:** automatically check Linux distribution,
   packages, graphical-session requirements, release availability and free space.
   Explain that the current account, password, files, applications and desktop
   remain available. Show download/storage requirements and actual limitations.
3. **Install Tilo Desktop:** install verified packages with authorization when
   required. Show real installation progress.
4. **Ready:** offer signing out to choose Tilo, with a save-work reminder. Do not
   sign out automatically. Explain how to select the previous desktop again.

No OS-account creation, migration selection, computer-name step, partitioning,
preparation password or Purpose screen. Existing files are not a migration job.
If no Tilo collaboration identity is available, offer connecting or creating it
inside the first Tilo session; reuse an existing identity where available.

## Your data: source-oriented selection

A source row shows its recognizable name/icon, meaningful counts and state.
Expanding it preserves the overview and reveals selectable children. Large
inventories remain responsive and do not require opening every row to accept
the default selection. Permission requests are scoped and understandable;
restricted paths remain unavailable until access is granted.

Required source structures include:

| Source | Overview | Expansion |
| --- | --- | --- |
| Each installed browser | Browser name, profiles, website accounts with supported identity evidence | Profiles, websites/accounts and applicable browser data |
| Thunderbird | Accounts, folders and message counts | Individual accounts, folders, Local Folders, address books and calendars |
| GNOME Online Accounts | Providers and recognizable account identities | Accounts and enabled services, with destination support and reconnect needs |
| Steam | Games with discovered local state | Individual games, save locations, Proton-prefix state and relevant settings |
| Files and storage services | Recognizable folders, drives and cloud locations | Effective source paths, local/online status, destination and exclusions |

Google Chrome, LibreWolf and Ungoogled Chromium are separate sources, not one
anonymous “Browser data” bucket. Do not equate cookie counts, website domains or
profile existence with authenticated account counts. Where the identity is
unknown, use an accurate website-data description instead.

A found session, an exportable session and a working destination session are
separate states. Do not promise every website remains signed in. Browser
bookmarks, history, saved passwords, extensions, settings and sessions each need
an explicit supported behavior; do not silently omit them behind one total.

The same account may be found in GNOME and Thunderbird. Preserve provenance,
deduplicate destination account configuration where appropriate, and do not
mistake two independently stored mail collections for duplicates. Preserve
local-only mail; server availability is not evidence that it exists remotely.

Steam Cloud labels do not prove uploaded durability or coverage of every save.
Check local saves, non-cloud titles, external libraries and Proton prefixes.
Copying only installed game files or Steam userdata is not complete coverage.
Unknown save locations must be reported, not treated as zero saved progress.

### Files, cloud storage and overlap

Resolve redirected folders and actual storage locations before summarizing.
Documents may be inside OneDrive; selecting both must not schedule two copies.
Other drives and network locations require explicit access and availability.
Never silently traverse links outside granted scope.

For every source record:

- Actual source identity and path, effective selected coverage and destination.
- Local bytes, online-only content and any unknown size/count.
- Copy, supported connection, reconnect or explicitly unresolved behavior.
- Access failures, changing content, name/metadata limitations and conflicts.

Example: an 82 GB OneDrive contains 14 GB locally. Explain both quantities.
Offer local copy plus a destination connection only if that integration works.
Otherwise offer downloading the remaining content before copying or explicitly
leaving it out. Do not hydrate large selections without disclosing download and
space consequences. Never report all 82 GB migrated after copying only 14 GB.

Use stable source/content identity to deduplicate known overlap. Do not silently
merge different files based on matching names. Preserve originals. Report
pending and failed items independently from verified completion. Optional
connected-folder behavior remains unavailable until its synchronization,
conflict and deletion-recovery contract is implemented and qualified.

## Hardware and installed-system quality

A primary reference class is an ordinary UEFI/ACPI laptop with Intel/AMD
integrated graphics and NVIDIA/AMD discrete graphics. Also qualify representative
ARM laptops against their actual model-specific components. One passing model
is not blanket support for either class.

Validate normal use, not just device enumeration: acceleration, GPU selection,
external displays and docks, brightness, suspend/resume, lid behavior, Wi-Fi
reconnection, Bluetooth, camera, microphone/headsets and battery/idle power.
Ship applicable audio routing, firmware and validated model-specific speaker
profiles. An enumerated sound device does not establish usable sound quality.

Compatibility results need applicability to the installed kernel, drivers,
firmware and release. Keep unknown support explicit. A supported model with a
known failing essential component is not a successful golden-path install.

## Completion, interruption and security

- Resume verified downloads and retain choices after closing setup. Do not claim
  disk mutation can resume merely because a progress flag was saved.
- Before mutation, cancellation preserves the existing system and safely removes
  temporary work. During mutation, expose the operation's actual safe boundary.
- Exercise failed one-time boot and both OS boot paths. Keep recovery material
  until installed data and the retained boot path have been verified.
- Privileged helpers revalidate disk identity, plan revision and permissions.
  Display-only facts and model lookups are not installation authority.
- Hardware-backed handoff is bound to the actual destination and intended boot
  policy, with a defined unlock/recovery path. Never seal another computer's
  secrets to the preparation computer and imply they will work on the target.
- Preserve canonical authentication, grants, sealed capabilities and source
  custody. No root credential-vault adoption, arbitrary target-user reassignment,
  unsigned payloads or plaintext private-state handoffs.

## Acceptance

A release must pass all three journeys using its packaged artifacts, not a demo
or a manually repaired developer checkout. Exercise realistic small windows,
scaling, keyboard navigation, screen-reader state and light/dark/high-contrast
presentation. Stable layouts must hold while typing and updating selections.

For data migration, first compare discovery against an independently established
source baseline: installed applications/profiles, configured accounts, actual
mail collections, storage locations and local game saves. An importer inventory
cannot prove its own completeness. Recognized applications without an importer
must remain visible as unsupported sources, not disappear from the overview.
Then reconcile the whole authorized source inventory against the result: each
source has verified destination content, an explicit user exclusion, or a
specific unresolved limitation. Validate opening files, reading local mail,
using accounts, browser behavior and loading saved games after cold boot.
Counts or a successful export alone do not establish a complete migration.

See [delivery gaps](installer-delivery-gaps.md) for the work and evidence still
required before this experience can be described as ready to ship.
