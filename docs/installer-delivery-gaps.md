# Installer delivery gaps

## Readiness

**Not ready to ship the complete specified experience.** The target is defined
in [Installation experience](installer-experience.md). This assessment covers the
implementation inspected at commit `1ec8fcf038`, plus the known acceptance gaps.
It is not a fresh hardware inventory or a claim that every backend was audited.
An item marked unverified is a delivery blocker, not proof that no code exists.

Substantial installation, migration, account-custody and UI code exists. Recent
focused interaction/widget tests and desktop smoke tests passed; those do not
establish a working packaged OS installation or complete data migration. The
older [migration status](migration-status.md) is historical and must not be used
to infer current Windows capabilities.

## Release and end-to-end blockers

| Gap | Current evidence | Suggested completion and proof |
| --- | --- | --- |
| Installable production payloads | `apps/imager/release-catalog.json` is empty. Guided Desktop also has a release-catalog requirement. A raw development build is not a complete distribution. | Produce authenticated OS/desktop payloads and matching catalogs through the release pipeline. Install using the actual resulting package, without test-catalog overrides or manually supplied dependencies. |
| Current-source OS round trip | Historical native fixtures and staging successes exist; a complete current-source install, installed-user adoption, data verification and cold boot are not established. | Run the installer-driven disposable VM journey with real disk backing, then representative hardware. Verify the installed system, retained OS, startup default and cleanup. Do not reuse old receipts to qualify new code. |
| Windows native acceptance | Existing Windows USB-free staging is real functionality and must be retained. Portable tests/cross-builds do not qualify current native execution; the future native workflow remains disabled. | Exercise Windows probing, encryption/Fast Startup conditions, shrinking, internal staging, ARM64/x86 boot paths and both OS boots on owned test machines. Enable distribution only for proven combinations. |
| Packaged dependency completeness | The missing pairing helper was repaired in packaging and rebuilt locally. That does not prove every shipped format includes compatible helpers. | Install each advertised package on a clean host; exercise pairing, privileged staging, extraction and runtime acquisition with no checkout or developer tools available. |
| Broad regression coverage | Focused UI tests passed, but broad native suites and strict backend checks have unresolved failures or incomplete evidence. | Diagnose failures against their actual base; repair regressions and fixtures without weakening custody, signatures, assertions or qualification gates. Keep source-bound results for the candidate. |

## Screen flow and selection

| Gap | Suggested implementation | Acceptance evidence |
| --- | --- | --- |
| Purpose remains in the current journey model | Remove the question from the ordinary install flow. Preserve safe existing system defaults; do not introduce a hidden behavior change while deleting the screen. | All three real routes have the specified screen order and no purpose prompt. |
| Current preparation password contradicts the intended first-boot flow | Replace the unconditional preparation-passphrase dependency with the protected handoff/OOBE design below. Do not simply lower a validator or leave secrets unencrypted. | Interactive installation has no extra long-password gate, credentials remain protected, and the first-boot password works. |
| Your data is not yet the unified source tree | Replace the category-led overview and separate login-only experience with app/source rows, nested selection and a shared toggle-all. Reuse existing discovery and custody services. | Select/exclude a website account, one Thunderbird account, one mail folder and a file subtree through both semantic actions and the actual UI. |
| Selection semantics across providers are incomplete | Represent selected, excluded, mixed, unavailable and newly discovered children explicitly. Preserve exclusions during refresh; revalidate stale batches atomically. | Partial permissions, refreshed inventories and source disappearance do not broaden selection or partially apply a stale toggle. |
| Device illustration was removed rather than resolved | Decide its useful role in identity approval. Where it explains an actual relationship, render authenticated participating devices in a stable layout; do not restore an invented fleet. | Names of different lengths and live approval updates do not move controls; no unverified device is presented as belonging to the identity. |
| Another-computer flow is not established as the full specified journey | Add destination-model/report selection, explicit data-source choice, supported preparation methods and destination continuation. Keep destination storage approval on the actual destination unless a validated unattended plan exists. | Prepare on one computer, continue on a different one without repeating completed choices or importing the preparation computer's data accidentally. |
| Desktop contract needs packaged first-session proof | Keep the existing direct native desktop route; verify existing-account reuse and move any genuinely needed collaboration identity setup into first session. | Add Tilo to a clean supported Linux installation; retain the account, files, apps and previous session; switch into and back out of Tilo. |
| Complete usability evidence | Extend the recent focused tests to the new source tree, compatibility report, OOBE and destination continuation. | Keyboard and assistive access, mixed selection announcements, small/high-DPI windows, all supported themes, stable typing and responsive large inventories. Physical fractional-scaling text quality remains unverified. |

## Application and data coverage

The requirement is completeness against the authorized source inventory, not a
claim that every possible third-party format is supported. Every discovered
source must have a supported result, an explicit exclusion, or a named gap.

| Source/work | What is still missing or unverified | Suggested approach |
| --- | --- | --- |
| Browsers | Dedicated source-grouped login selection exists, but its current supported service handling is not equivalent to a complete browser/profile/website inventory. Full Chrome, LibreWolf and Ungoogled Chromium coverage is not established. | Build a per-browser/profile capability matrix covering bookmarks, history, passwords, extensions, settings and website sessions. Count accounts only where identity evidence exists; test actual destination use, including reconnect failures. |
| Thunderbird | No complete account/folder/message transfer and destination-use proof is established by the inspected installer sources. Finding a mail directory or account configuration is insufficient. | Inventory accounts, IMAP caches, local-only mail, Local Folders, address books and calendars. Preserve flags, attachments, folder hierarchy and offline-only content. Verify readable destination messages and representative attachments; do not substitute downloading server mail for preserving local mail. |
| GNOME Online Accounts | Canonical discovery/account-transfer code exists. Complete enabled-service mapping and usable destination accounts are unverified. | Enumerate each configured provider and enabled service; deduplicate destination account setup while retaining application provenance. Use canonical credential access and explicit reconnects, not root vault adoption. Verify files/mail/calendar separately after install. |
| Steam | Windows library/userdata inventory exists. Complete Linux/Windows save discovery, external-library handling and Proton-prefix transfer are not qualified. | Inventory libraries, per-user state, app-specific save locations, Proton prefixes and relevant configuration. Distinguish reinstallable game assets from irreplaceable saves. Validate non-cloud saves by loading games after migration. Unknown save locations remain visible. |
| Local files | Verified-copy and custody foundations exist, but full current-source installed-target reconciliation is missing. | Test redirected folders, overlapping roots, multiple drives, links, permissions, filenames, metadata, changing files and interrupted copying. Compare content against the selected inventory; open representative installed files. |
| OneDrive and other cloud-backed folders | Windows placeholder/reparse exclusions and account hints exist. End-to-end hydration, destination service behavior, overlap handling and truthful total/local accounting are incomplete or unverified. | Reconcile redirected folders with provider roots, distinguish local bytes from remote content and disclose hydration costs. Offer only proven destination connections; otherwise explicitly download-and-copy or leave unavailable content out. Verify counts and data, not just account configuration. |
| Cross-source duplicates | A source-oriented identity/coverage model spanning accounts, folders and providers is not established. | Share provenance and effective-coverage records across importers. Avoid duplicate account creation and overlapping file copies without merging different same-name content or independent mail collections. |
| Pending data and first-boot usability | OAuth success, a prepared export and a copied tree do not prove files, mail, calendars or sessions are usable. | Record per-source receiving outcomes and show pending/reconnect states after first boot. Verify actual app use; never label migration complete from enumeration or token acquisition alone. |
| Connected folders | The complete user-facing synchronization service remains unfinished/unqualified. | Keep it unavailable until comparison, conflicts, deletion recovery, interruption and native filesystem proofs pass. A truthful verified copy is preferable to advertising unproven synchronization. |

## Passwords, destination protection and unattended setup

The existing implementation explicitly requires preparation passphrases in
`crates/platform/imager-backend/src/startup_password.rs`; additional encrypted
transfer paths enforce a 15-character minimum. This is a backend dependency,
not only a label or front-end validation issue.

Missing work:

1. Separate local login credentials, disk-unlock/recovery credentials, Tilo
   identity protection and temporary migration protection. Specify which exist
   for each route and who can unlock them at each step.
2. Check the actual destination's hardware protection and intended installed
   boot policy. TPM presence alone is not a working cross-reboot secret handoff;
   a preparation machine cannot attest another computer by model name.
3. Design and implement authenticated, destination-bound protection for identity
   keys, prepared data and account credentials, including unlock after the real
   installed boot and an explicit recovery path. Test policy changes and failure.
4. For interactive installation without usable automatic protection, prefer
   deferring sensitive capture/connection until first boot or an authenticated
   source-to-target transfer. If another secure mechanism is needed, specify it
   before implementation; do not silently add a second password questionnaire.
5. Make first-boot account creation consume completed choices and ask for the
   normal login password once. Installed-user adoption currently assumes UID
   1000; arbitrary user mapping requires authenticated identity transport, not
   an editable UID field.
6. Complete explicit unattended credentials and disk-plan input, secure secret
   delivery and actual-target preflight. Provide interactive continuation with
   clear errors. Test missing protection, mismatched hardware/disks, failed
   unlock and absent input devices/remote connectivity without leaking secrets.

Suggestion: resolve and prove this handoff design before removing password
checks from production. Preserve existing sealed capabilities, original-caller
binding and receiving/source proofs while changing the user interaction.

## Hardware compatibility and everyday quality

An install-feasibility probe exists; it is not a complete component-level support
report or a model catalog. No fresh physical qualification of the reference
laptop or representative ARM laptops is established here.

Required work and suggested evidence:

- **Evidence-backed compatibility:** associate model/variant and component IDs
  with applicable OS/kernel/driver/firmware evidence. Expire or recheck results
  when that applicability changes. Show unknown rather than optimistic support.
- **Hybrid graphics:** test Intel/AMD integrated graphics with NVIDIA/AMD discrete
  graphics; acceleration, GPU selection/offload, external ports/docks, suspend,
  brightness and idle power. Cover supported secure-boot and driver-signing paths
  without treating security disablement as a general fix.
- **Ordinary ACPI laptop behavior:** lid close/open, suspend/resume, battery
  reporting, charging, Wi-Fi reconnect, Bluetooth, cameras, touchpads and input.
- **ARM variants:** test the actual model's boot, display, networking, storage,
  audio, fingerprint reader and power behavior. Architecture compatibility is
  not a substitute for these results.
- **Sound quality:** ship applicable topology, firmware, routing and validated
  speaker profiles. Measure/listen on actual models, verify microphone/headset
  behavior and safe gain. Do not claim good audio because a device enumerates.
- **Whole installed-system acceptance:** exercise these behaviors after the
  packaged installation, not only from an unrelated live image or a developer
  session. Confirm the retained OS still boots and its startup default stays.

## Recommended delivery order

First establish a real packaged install and cold-boot route with authenticated
payloads. In parallel, build the complete authorized source inventory and its
source-oriented selection UI. Resolve the destination-protection/OOBE dependency
before removing the existing preparation-password boundary. Then reconcile the
installed data and qualify everyday hardware use across the reference class.

The reference laptop is a full acceptance case, not an excuse to hard-code its
paths or omit another machine's sources. Add representative hybrid-GPU and ARM
machines and realistic source fixtures. A clean newly created profile with no
mail, cloud placeholders or local game saves cannot substitute for this test.

Ship a route only after its actual package completes its journey. Keep known
limitations specific and visible; do not use a generic success badge, passing
unit-test count or this document as evidence of readiness.
