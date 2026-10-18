# Windows preparation never enrolls identities or signs into accounts.
installer-windows-heading = Prepare on Windows
installer-windows-intro = Choose the folders you want to keep. Local files are selected for a verified backup by default, including unknown game folders and mods. Originals stay unchanged; cloud copies are not verified.
installer-windows-find = Suggest locations
installer-windows-source-title = Select one exact source folder
installer-windows-grant = Review this folder
installer-windows-choose-known-folder = Choose a personal folder
installer-windows-choose-onedrive = Choose a OneDrive folder
installer-windows-choose-outlook = Choose Outlook archives or local stores
installer-windows-choose-steam = Choose Steam data
installer-windows-choose-steam-library = Choose another Steam library
installer-windows-choose-other = Choose another folder
installer-windows-budget = Known sizes — copy: { $copy }; retained at source: { $retain }; explicitly excluded: { $excluded }; unavailable locally: { $unavailable }
installer-windows-incomplete = This inventory is incomplete. Unavailable, unsupported or unreadable data remains at source. Do not replace Windows based on this export.
installer-windows-grant-library = Review the referenced library: { $path }
installer-windows-category-summary = { $category }: { $count } entries, { $size } known ({ $unknown } sizes unknown); { $unavailable } unavailable — { $decision }
installer-windows-empty = No files were found in this selected folder.
installer-windows-open-source = Open source folder
installer-windows-open-pending = A source folder is already opening.
installer-windows-open-failed = The source folder could not be opened. Check that it is still available and try again.
installer-windows-gaps = { $count } parts could not be fully checked. Keep the originals.
installer-windows-show-paths = Review source paths
installer-windows-hide-paths = Hide source paths
installer-windows-path = { $path } — { $size }; { $availability }
installer-windows-size-unknown = size unknown
installer-windows-availability-local = available locally
installer-windows-availability-placeholder = online-only; not copied
installer-windows-availability-unsupported = cannot safely copy; keep the original
installer-windows-gap-path = Not fully checked: { $path }
installer-windows-plan-copy = Copy available local files under { $destination } in the chosen backup, keeping their folder layout. Unavailable files stay at source.
installer-windows-backup-plan = Choose a backup destination next. This preserves files; it does not import them into apps or authorize replacing Windows. Keep the backup on a different drive from the USB image target.
installer-windows-plan-retain-original = No backup copy: keep these files at the source. Do not erase that drive.
installer-windows-plan-exclude = Not included in the backup by your explicit choice. Originals remain unchanged.
installer-windows-exclude = Exclude with the stated reason
installer-windows-exclusion-reason = Reason for excluding data
installer-windows-hints-unavailable = Account suggestions could not be checked. Local folder review is unaffected; sign in manually in the live system.
installer-windows-copy = Copy
installer-windows-retain-original = Retain at source
installer-windows-password = Password for private sign-in suggestions
installer-windows-password-required = Use at least eight characters to encrypt suggestions for the live session.
installer-windows-export-title = Select a backup destination, separate from the image target
installer-windows-export = Export reviewed preparation
installer-windows-checking = Checking only the independently selected locations…
installer-windows-exporting = Copying and verifying selected local files…
installer-windows-exported = Verified local copies and encrypted suggestions were exported. Keep this backup separate from the drive you erase in the image writer. Source data is unchanged; sign in later from live Tilo.
installer-windows-export-failed = Export is incomplete. Verified output is retained. Keep the originals; this is not a replacement authorization.
installer-windows-source-failed = This exact source could not be reviewed. Select it again.
installer-windows-decision-failed = Review the source decision. Exclusion requires an explicit reason.
installer-windows-media = Open the verified USB image writer
installer-windows-category-personal-files = Documents and personal files
installer-windows-category-sync-data = Sync folder data (sync unverified)
installer-windows-category-mail-archive = Mail archives
installer-windows-category-mail-cache-or-drafts = Local mail stores and drafts
installer-windows-category-steam-userdata = Steam saves and user data (cloud copy unverified)
installer-windows-category-game-save = Game saves
installer-windows-category-game-mod = Mods and workshop data
installer-windows-category-game-installation = Game installations (may contain local changes)
installer-windows-category-potentially-unique = Other potentially unique data

# Image write review. The remaining image contents are written unchanged.
imager-review-setup-hostname = Written with hostname { $hostname }, then verified

# Guided setup semantic resource and action labels.
imager-setup-action-choose-experience = Choose setup experience
imager-setup-placeholder-experience = desktop
imager-setup-param-experience = Available setup experience
imager-setup-action-set-common-mode = Set common folder mode
imager-setup-action-set-folder-mode = Set folder mode
imager-setup-placeholder-folder-mode = copy
imager-setup-param-folder-mode = Folder behavior
imager-setup-param-common-folder-mode = Folder behavior applied to folders without an override
imager-setup-action-use-common-mode = Use the common folder mode
imager-setup-action-remove-folder = Remove folder from setup
imager-setup-action-select-suggestion = Select suggested folder
imager-setup-action-choose-folder = Choose another folder
imager-setup-action-toggle-help = Show or hide folder help
imager-setup-action-next = Continue setup
imager-setup-action-back = Go back in setup
imager-setup-action-open-imports = Open folder imports
imager-setup-action-install-desktop = Install Tilo Desktop
imager-setup-action-cancel-desktop-download = Cancel Desktop download
imager-setup-action-retry-desktop = Retry Desktop host check
imager-setup-action-start-download = Download Tilo
imager-setup-action-pause-download = Pause download
imager-setup-action-resume-download = Resume download
imager-setup-action-cancel-download = Cancel download
imager-setup-action-discard-download = Discard unfinished download data
imager-setup-action-release-download = Keep download for later

imager-setup-error-no-experience = No setup experience is available on this computer
imager-setup-error-unknown-experience = Unknown setup experience: { $value }
imager-setup-error-unknown-mode = Unknown folder mode: { $value }
imager-setup-error-invalid-operation = Invalid setup operation: { $error }
imager-setup-error-folder-choices-closed = Folder choices are not open on the current setup page
imager-setup-error-folder-selection-stale = The folder selection is no longer available
imager-setup-error-connected-unavailable = Connected folders are not available on this computer
imager-setup-error-wait-for-scans = Wait for the selected folders to finish scanning
imager-setup-error-suggestion-unavailable = That suggested folder is unavailable on this computer
imager-setup-error-picker-unavailable = The folder picker is already open or folder choices are not available
imager-setup-error-picker-open = The folder picker is already open
imager-setup-error-help-not-on-page = Folder help is available on the folder page
imager-setup-error-cannot-continue = Complete the current setup choice before continuing
imager-setup-error-at-start = Setup is already on its first page
imager-setup-error-desktop-install-unavailable = Desktop installation is not available in the current setup state
imager-setup-error-desktop-download-not-active = The Desktop package download is not active
imager-setup-error-desktop-retry-unavailable = The Desktop host check cannot be retried in the current setup state
imager-setup-error-download-start-unavailable = A Full Tilo download cannot start in its current state.
imager-setup-error-download-review-only = Full Tilo downloads can start only from the Full Tilo review.
imager-setup-error-download-pause-unavailable = The download is not currently active
imager-setup-error-download-resume-unavailable = This saved download cannot be resumed yet
imager-setup-error-download-cancel-unavailable = The download is not currently active
imager-setup-error-download-discard-unavailable = Unfinished download data is not available to discard
imager-setup-error-download-release-unavailable = The verified download is not available to keep for later

imager-setup-resource-root = Guided Tilo setup
imager-setup-resource-selection = Current setup selection
imager-setup-resource-review = Setup review state
imager-setup-resource-availability = Current setup availability
imager-setup-resource-selected-folder = Selected folder { $number }
imager-setup-resource-suggested-folder = { $folder } folder
imager-setup-resource-desktop = Tilo Desktop installation
imager-setup-resource-internal-download = Full Tilo download
imager-setup-badge-selection = Selection
imager-setup-badge-review = Review
imager-setup-badge-availability = Availability
imager-setup-badge-folder = Folder { $number }
imager-setup-badge-suggestion = Suggested folder
imager-setup-badge-desktop = Desktop
imager-setup-badge-download = Download
imager-setup-badge-page-experience = Experience
imager-setup-badge-page-folders = Folders
imager-setup-badge-page-review = Review
imager-setup-badge-page-setup = Setup
imager-setup-suggestion-available = Available to select
imager-setup-suggestion-unavailable = Unavailable on this computer

imager-setup-detail-folder-common = { $mode }, the common choice
imager-setup-detail-folder-override = { $mode }, chosen for this folder
imager-setup-detail-desktop = { $status }
imager-setup-detail-internal-download = { $status }
imager-setup-detail-selection =
    experience { $experience ->
        [none] no experience selected
       *[other] { $experience }
    } · common { $common_mode } · { $folder_count } folders · { $active_folder_count } active · help { $help ->
        [true] open
       *[false] closed
    } · imports { $imports ->
        [true] open
       *[false] closed
    }
imager-setup-detail-review =
    ready { $ready ->
        [true] yes
       *[false] no
    } · current page { $page }
imager-setup-detail-availability =
    Desktop { $desktop ->
        [true] yes
       *[false] no
    } · Full OS { $full_os ->
        [true] yes
       *[false] no
    } · Connected { $connected ->
        [true] yes
       *[false] no
    } · Next { $next ->
        [true] yes
       *[false] no
    } · Back { $back ->
        [true] yes
       *[false] no
    } · Folder policies { $folder_policies ->
        [true] yes
       *[false] no
    } · Folder edits { $folder_edits ->
        [true] yes
       *[false] no
    } · Help { $help ->
        [true] yes
       *[false] no
    } · Imports { $open_imports ->
        [true] yes
       *[false] no
    }
imager-setup-detail-root =
    { $page } · { $experience ->
        [none] no experience selected
       *[other] { $experience }
    } · { $common_mode } · { $folder_count } folder(s) · review { $review ->
        [true] yes
       *[false] no
    } · Desktop { $desktop ->
        [true] yes
       *[false] no
    } · Full OS { $full_os ->
        [true] yes
       *[false] no
    } · Connected { $connected ->
        [true] yes
       *[false] no
    } · Next { $next ->
        [true] yes
       *[false] no
    } · Back { $back ->
        [true] yes
       *[false] no
    }

# Folder import semantic resource and action labels.
imager-import-resource-root = Folder imports
imager-import-resource-operation = Folder import
imager-import-resource-folder = Saved folder { $number }
imager-import-badge-root = Imports
imager-import-badge-operation = Copy
imager-import-badge-folder = Saved folder
imager-import-action-choose-source = Choose source folder
imager-import-action-read-status = Read current import status
imager-import-action-choose-destination = Choose destination folder
imager-import-action-clear-selection = Clear folder selection
imager-import-action-prepare-review = Prepare copy review
imager-import-action-check-identity = Check identity setup
imager-import-action-start-reviewed-folders = Copy reviewed folders
imager-import-action-later-all = Decide later for all folders
imager-import-action-start = Start copy
imager-import-action-retry = Continue copy
imager-import-action-cancel = Request to stop copy
imager-import-action-toggle-details = Show or hide copy details
imager-import-action-next-issues = Load next issue page
imager-import-action-first-issues = Return to first issue page
imager-import-action-review-destination = Review destination
imager-import-action-choose-folder-destination = Choose another destination
imager-import-action-folder-copy = Use one-time Copy
imager-import-action-folder-later = Decide later
imager-import-action-folder-connected = Use Connected folders
imager-import-error-invalid-operation = Invalid folder import operation: { $error }
imager-import-error-operation-kind = Folder import operations are local view operations
imager-import-error-picker-unavailable = A folder chooser is already open or unavailable
imager-import-error-selection-empty = No folder selection is available to clear
imager-import-error-review-unavailable = Choose a source and destination before preparing a review
imager-import-error-identity-check-unavailable = Identity setup is already ready or being checked
imager-import-error-batch-unavailable = No reviewed Copy folder is ready to start
imager-import-error-later-all-unavailable = Saved folder choices cannot change while a copy is running
imager-import-error-start-unavailable = This copy is not ready to start
imager-import-error-retry-unavailable = This copy cannot be continued in its current state
imager-import-error-cancel-unavailable = This copy is not currently running
imager-import-error-details-unavailable = This copy has no details to show
imager-import-error-next-issues-unavailable = Another issue page is not available
imager-import-error-first-issues-unavailable = The first issue page is not available
imager-import-error-folder-review-unavailable = This saved folder cannot be reviewed in its current state
imager-import-error-folder-mode-unavailable = Saved folder choices cannot change while a copy is running
imager-import-error-connected-unavailable = Connected folders are not available on this computer
imager-import-error-stale-operation = That folder import is no longer available
imager-import-error-stale-folder = That saved folder choice is no longer available
imager-import-error-command-refused = Folder import could not continue. Open import status (read-status) on this computer for details
imager-import-detail-root = { $operations ->
        [one] One copy
       *[other] { $operations } copies
    }
imager-import-detail-operation = { $state ->
        [awaiting_review] Waiting for review
        [ready] Ready to start
        [running] Copying
        [interrupted] Interrupted
        [cancelled] Stopped
        [incomplete] Needs attention
        [failed] Could not start
       *[complete] Complete
    }, { $files ->
        [one] one file checked
       *[other] { $files } files checked
    }
imager-import-detail-folder = { $mode ->
        [later] Decide later
        [connected] Connected
       *[copy] Copy
    }{ $reviewed ->
        [true] , destination reviewed
       *[false] {""}
    }

# Full Tilo installed alongside the current system, without a USB drive.
imager-setup-resource-internal = Full Tilo installation
imager-setup-badge-internal = Full Tilo
imager-setup-detail-internal = { $readiness }
imager-setup-action-internal-set-target-size = Set Tilo size
imager-setup-param-internal-target-gib = Tilo size in GiB
imager-setup-action-internal-install = Install Tilo alongside the current system
imager-setup-action-internal-restart = Restart into the Tilo installer
imager-setup-action-internal-restart-later = Restart later
imager-setup-action-internal-retry = Start the Tilo installer on the next restart
imager-setup-action-internal-remove = Remove the Tilo installer
imager-setup-action-internal-firmware-settings = Restart to firmware settings
imager-setup-action-internal-disable-fast-startup = Turn off Fast Startup
imager-setup-action-internal-recheck = Check this computer again
imager-setup-internal-error-not-review = Full Tilo installation commands are available on the Full Tilo review.
imager-setup-internal-error-command-unavailable = That command is not available in the current installation state.
imager-setup-internal-error-unavailable = Full Tilo installation is not available on this computer.
imager-setup-internal-error-size-range = Choose a Tilo size between { $min } and { $max } GiB.
imager-setup-internal-error-no-plan = There is no installation plan for this disk yet.
imager-setup-internal-error-plan-changed = The installation plan changed. Review it again before installing.
imager-setup-internal-error-blocked = Something still blocks installation. Resolve the items under Before installing.
imager-setup-internal-error-download = Download and verify Tilo before installing.
imager-setup-internal-error-running = The Tilo installer helper is already running.
imager-setup-internal-error-staged = The Tilo installer is already set up. Restart to start it.
imager-setup-internal-error-no-pending = There is no unfinished Tilo installer on this computer.
imager-setup-internal-error-pending-changed = That unfinished Tilo installer is no longer the current one.
imager-setup-internal-error-no-restart = The Tilo installer is not set up to start yet.
imager-setup-internal-error-remedy-unavailable = That fix does not apply to this computer right now.
imager-setup-internal-checking =
    Checking whether Tilo can be installed alongside { $host ->
        [windows] Windows
        [linux] your Linux system
       *[other] your current system
    } on this computer…
imager-setup-internal-gated = Full Tilo is not available on this computer yet.
imager-setup-internal-no-release = There is no Full Tilo release for this computer's processor yet.
imager-setup-internal-unsupported = Tilo cannot be installed alongside the current system on this computer: { $reasons }
imager-setup-internal-ready =
    Tilo can be installed alongside { $host ->
        [windows] Windows
        [linux] your Linux system
       *[other] your current system
    } on this computer.
imager-setup-internal-plan-title = Installation plan
imager-setup-internal-plan-checking = Checking this computer's disk…
imager-setup-internal-plan-shrink =
    { $host ->
        [windows] Windows (C:)
       *[other] Your system partition
    } shrinks by { $shrink }, from { $from } to { $to }.
imager-setup-internal-plan-free = Tilo uses free, unpartitioned space on this disk. No existing partition is resized.
imager-setup-internal-plan-target = Tilo gets { $target }.
imager-setup-internal-plan-staging = The Tilo installer uses another { $staging } while it runs. That space is given to Tilo after your first login.
imager-setup-internal-plan-error-space = There is not enough safe space on this disk. Tilo and its installer need { $needed }, but only { $available } can be made available.
imager-setup-internal-plan-error-other = No installation plan could be made for this disk: { $detail }
imager-setup-internal-size-label = Tilo size
imager-setup-internal-size-help =
    Choose between { $min } and { $max } GiB. More space for Tilo leaves less for { $host ->
        [windows] Windows
       *[other] the current system
    }.
imager-setup-internal-expect =
    { $host ->
        [windows] Windows stays installed and remains the startup default.
       *[other] Your current system stays installed and remains the startup default.
    } Installing takes one restart into the Tilo installer, where you confirm again before anything is installed. Secure Boot must be turned off.
imager-setup-internal-unverified = Back up anything important before you continue.
imager-setup-internal-blockers-title = Before installing
imager-setup-internal-blockers-none = Nothing on this computer blocks installation.
imager-setup-internal-blocker-not-uefi = This computer starts in legacy BIOS mode. Tilo needs UEFI startup.
imager-setup-internal-blocker-secure-boot-on = Secure Boot is on. Turn it off in this computer's firmware settings, then open setup again. Setup never changes Secure Boot keys.
imager-setup-internal-blocker-bitlocker-locked = The system drive is encrypted with BitLocker and its protection cannot be suspended for the restart.
imager-setup-internal-blocker-not-gpt = The system disk uses the older MBR layout. Tilo needs a GPT disk.
imager-setup-internal-blocker-disk-unsupported = This disk setup is not supported (for example Storage Spaces, a dynamic disk, RAID or a removable disk), or the disk could not be read.
imager-setup-internal-blocker-hibernation = Fast Startup or hibernation is on, so Windows does not fully shut down. Turn it off before installing.
imager-setup-internal-blocker-insufficient-space = There is not enough free space for Tilo and its installer, even after safely shrinking the system partition.
imager-setup-internal-blocker-on-battery = This computer is on battery below 50%. Plug it in before installing.
imager-setup-internal-blocker-pending-operation = An earlier Tilo installer setup is unfinished. Continue it or remove it below.
imager-setup-internal-blocker-arch-mismatch = No Tilo release matches this computer's processor.
imager-setup-internal-remedy-firmware = Restart to firmware settings
imager-setup-internal-remedy-fast-startup = Turn off Fast Startup
imager-setup-internal-recheck = Check again
imager-setup-internal-install = Install Tilo
imager-setup-internal-continue = Continue installation
imager-setup-internal-installing = Installing…
imager-setup-internal-run-title = Tilo installer
imager-setup-internal-run-waiting = Waiting for administrator approval…
imager-setup-internal-run-progress = { $step }: { $done } of { $total }
imager-setup-internal-run-step = { $step }…
imager-setup-internal-step-shrink = Shrinking the system partition
imager-setup-internal-step-create-esp = Creating the installer boot partition
imager-setup-internal-step-create-root = Creating the installer partition
imager-setup-internal-step-write-esp = Writing the installer boot files
imager-setup-internal-step-write-root = Writing the installer system
imager-setup-internal-step-verify = Checking what was written
imager-setup-internal-step-handoff = Saving your installation choices for the installer
imager-setup-internal-step-boot-entry = Adding the Tilo installer to the startup menu
imager-setup-internal-step-boot-next = Starting the Tilo installer on the next restart
imager-setup-internal-step-remove-boot-next = Clearing the one-time startup choice
imager-setup-internal-step-remove-boot-entry = Removing the Tilo installer from the startup menu
imager-setup-internal-step-remove-root = Removing the installer partition
imager-setup-internal-step-remove-esp = Removing the installer boot partition
imager-setup-internal-step-restore-shrink = Returning space to the system partition
imager-setup-internal-done-title = Ready to restart
imager-setup-internal-done =
    The Tilo installer starts once, the next time this computer restarts. { $host ->
        [windows] Windows stays the startup default. If you leave the installer, Windows starts as usual.
       *[other] Your current system stays the startup default. If you leave the installer, it starts as usual.
    }
imager-setup-internal-restart-now = Restart now
imager-setup-internal-restart-later = Restart later
imager-setup-internal-restart-deferred = Save your work and restart when you are ready. The Tilo installer starts on the next restart.
imager-setup-internal-restarting = Restarting…
imager-setup-internal-removed = The Tilo installer was removed and its space returned.
imager-setup-internal-resume-title = Unfinished Tilo installer
imager-setup-internal-resume = An earlier Tilo installer setup on this computer is unfinished or was not used. Continue finishes it with the verified download. Try again starts the Tilo installer on the next restart. Remove deletes the installer and returns its space.
imager-setup-internal-resume-continue = Continue
imager-setup-internal-resume-retry = Try again
imager-setup-internal-resume-remove = Remove Tilo installer
imager-setup-internal-failure-plan-stale = This computer's disk or startup settings changed after you reviewed the plan, so the installer helper stopped before changing anything. Review the updated plan, then install again.
imager-setup-internal-failure-boot-next-busy = Another one-time startup choice is already set on this computer, so setup did not replace it. Restart once to clear it, then choose Try again.
imager-setup-internal-failure-digest-mismatch = The downloaded files did not match their verified fingerprints, so nothing was written. Discard the download and download Tilo again.
imager-setup-internal-failure-launch = The installer helper could not start: { $detail }
imager-setup-internal-failure-interrupted = Setup stopped following the installer helper before it finished. It may still be running; open setup again to see where it stopped.
imager-setup-internal-failure-generic = The installer helper stopped: { $detail } Each step it finished is recorded, so you can continue or remove the Tilo installer.
imager-setup-internal-command-failed = That did not work: { $detail }
imager-setup-internal-status-checking = Checking this computer…
imager-setup-internal-status-installing = Preparing the Tilo installer…
imager-setup-internal-status-retrying = Setting the Tilo installer to start on the next restart…
imager-setup-internal-status-removing = Removing the Tilo installer…
imager-setup-internal-status-firmware = Restarting to firmware settings…
imager-setup-internal-status-fast-startup = Turning off Fast Startup. Approve the administrator prompt.
imager-setup-internal-status-size = Tilo size set to { $gib } GiB.
imager-setup-internal-download-consuming = Preparing the Tilo installer
imager-setup-internal-download-consuming-detail = The installer helper is copying the verified download to this disk.
imager-setup-internal-download-consumed = Download used
imager-setup-internal-download-consumed-detail = The installer helper copied the verified download. The download stays on this computer until you discard it.
imager-setup-internal-download-kept = Installation stopped; download kept

# Desktop setup shares these effects with its controls and automation.
imager-setup-desktop-heading = Tilo Desktop on this computer
imager-setup-desktop-install = Install Tilo Desktop
imager-setup-desktop-cancel = Cancel download
imager-setup-desktop-recheck = Check again
imager-setup-desktop-command-unavailable = This operation is unavailable in the current setup state.
imager-setup-method-heading = Install Tilo OS
imager-setup-method-body = Install alongside your current system, or create a USB installer. You review the target before writing.
imager-setup-usb-open = Create a USB installer
imager-setup-usb-unavailable = USB image writing is unavailable on this host. Use a supported host to create the installer.
imager-setup-alongside = Install directly alongside

imager-journey-heading = Make Tilo yours
imager-journey-body = Bring files now or decide later. Connect accounts explicitly and choose apps in Store after identity setup.
imager-journey-accounts = Connect accounts
imager-journey-store = Choose apps in Store
imager-journey-identity-required = Complete identity setup in the trusted Tilo session first.
imager-journey-navigation-pending = Asking the session to open your choice…
imager-journey-navigation-opened = Your choice opened.
imager-journey-navigation-lost = The session did not return an opening result. Try again.

imager-setup-alongside-unavailable = Not available on this computer yet.

## History: what changed in the imager's document
imager-select-source = Chose the image to write
imager-clear-source = Cleared the image to write
imager-install-profile-hostname = Changed the hostname
imager-install-profile-locale = Changed the locale
imager-install-profile-keyboard = Changed the keyboard layout
imager-install-profile-timezone = Changed the time zone
imager-install-profile-filesystem = Set the filesystem to { $new ->
    [btrfs] Btrfs
    [ext4] Ext4
   *[xfs] XFS
}
imager-install-profile-wifi-ssid = Changed the Wi-Fi network
imager-install-profile-device-label = Changed the new device's name
imager-install-profile-enroll-local-identity = { $new ->
    [true] Chose to enroll the new device
   *[false] Chose not to enroll the new device
}
imager-user-account-full-name = Changed the first user's full name
imager-user-account-username = Changed the first user's username

# Guided installer and explicit local migration.
installer-preview-destination-heading = Welcome to Tilo

installer-preview-destination-intro = Where would you like to install it?

installer-preview-computer-title = On this computer

installer-preview-computer-body = Add Tilo, the system that runs your apps, alongside your current system. You’ll review the disk changes before installing.

installer-preview-computer-usb-body = Make a USB installer, then restart this computer from it to add Tilo alongside your current system. You’ll need a USB drive you can erase.

installer-preview-another-title = On another computer

installer-preview-another-body = Make a USB installer to take to another computer. You’ll need a USB drive you can erase; the other computer must be able to start from it.

installer-preview-desktop-title = As a desktop

installer-preview-desktop-body = Use the Tilo desktop with your existing Linux system.

installer-preview-migration-heading = Bring your things, or start fresh?

installer-preview-migration-intro = First we’ll set up your name and identity. Then you can choose what to bring and approve access. Nothing is installed at this step.

installer-preview-migrate-portable-body = Bring selected files, supported apps and accounts to the other computer. Your originals stay here. For large files, you may need to restart this computer from the USB drive to copy them before taking it away.

installer-preview-migrate-title = Migrate from this computer

installer-preview-migrate-body = Make your documents, supported apps and accounts accessible in Tilo. Your originals stay unchanged.

installer-preview-fresh-title = Start fresh

installer-preview-fresh-body = Start without importing your documents, apps or accounts. Your existing things stay unchanged.

installer-preview-identity-heading = Have you used Tilo before?

installer-preview-new-title = This is my first time

installer-preview-new-body = Create your Tilo identity. Welcome!

installer-preview-existing-title = I already use Tilo

installer-preview-existing-body = Connect this device to your existing Tilo identity.

installer-preview-profile-heading = Your profile

installer-preview-profile-intro = This is how you'll appear across Tilo.

installer-preview-name-label = Your name

installer-preview-profile-avatar = Keep your initials or choose a picture. You can change it later.

installer-preview-password-heading = Choose a password

installer-preview-password-intro = You’ll use this to unlock Tilo.

installer-preview-password-label = Password

installer-preview-confirmation-label = Confirm password

installer-preview-password-hint = Use at least 15 characters, ideally several unrelated words.

installer-preview-connect-heading = Bring your Tilo identity

installer-preview-device-title = Use another Tilo device

installer-preview-device-body = Confirm the connection on a device you already use.

installer-preview-backup-title = Restore an identity backup

installer-preview-backup-body = Use your encrypted backup and its passphrase.

installer-preview-purpose-heading = How will this device be used?
installer-preview-purpose-intro = The same Tilo OS, with defaults for the job it will do.
installer-preview-purpose-computer-title = Computer
installer-preview-purpose-computer-body = For everyday use. Choose when it sleeps in Power settings; your approved devices sync while it is awake.
installer-preview-purpose-server-title = Always-available server
installer-preview-purpose-server-body = Keeps running when idle so your approved devices can sync. Screen blanking, locking, lid and low-battery protection stay on. Use reliable power; this does not grant remote access or enable nearby-device discovery.

installer-preview-devices-heading = Your devices

installer-preview-devices-intro = Use this identity to manage all your Tilo devices.

installer-preview-device-name-label = Name this device

installer-preview-device-default = My computer

installer-preview-more-devices = More devices

installer-preview-existing-avatar = Your existing Tilo identity

installer-preview-new-recovery = When you first log in, Tilo will help you make an encrypted identity backup.

installer-preview-existing-recovery = Your existing identity stays yours. This device joins it; you don't create a second identity.

installer-preview-belongings-heading = Your belongings

installer-preview-belongings-intro = Choose what to bring. Your originals stay unchanged.

installer-preview-files-title = Files and settings

installer-preview-files-body = Your entire home folder, including hidden files, browser profiles, saved credentials, Flatpak apps and settings.

installer-preview-accounts-title = Connect services in Tilo

installer-preview-accounts-body = Choose saved connections or sign in on the next step.

installer-preview-choose-belongings = Choose what comes over

installer-preview-hide-details = Hide details

installer-preview-bring-things = Bring my things

installer-preview-bring-selected = Bring selected things

installer-preview-storage-heading = Choose space for Tilo

installer-preview-storage-intro = Keep your current system and install Tilo alongside it.

installer-preview-storage-another = Choose a USB drive next. Its contents will be erased when you confirm the write.

installer-preview-storage-desktop = Your existing Linux system and files stay in place.

installer-preview-review-heading = Review your setup

installer-preview-review-intro = Select an item to change it.

installer-preview-review-migrate = Bring the selected belongings from this computer. Your originals stay unchanged.

installer-preview-review-fresh = Nothing is imported from this computer.

installer-preview-ready-heading = Your setup choices are ready

installer-preview-ready-intro = Nothing has been installed.

installer-preview-continue = Continue

installer-preview-back = Back

installer-preview-finish = Finish preview

installer-preview-restart = Try another path

installer-preview-choose-first = Choose an option to continue.

installer-preview-name-required = Enter the name you'd like to use in Tilo.

installer-preview-device-required = Give this device a name.

installer-preview-password-short = Use at least 15 characters.

installer-preview-password-spaces = Remove spaces at the beginning or end.

installer-preview-password-common = Use several unrelated words.

installer-preview-password-numeric = Use at least 20 digits, or add letters.

installer-preview-password-repetitive = Avoid repeated characters or patterns.

installer-preview-password-invalid = Choose a different password.

installer-preview-password-mismatch = The passwords don't match.

installer-preview-identity-busy = Wait for preparation to finish.

installer-preview-identity-preparing = Preparing your identity…

installer-preview-identity-prepare-failed = Your identity couldn't be prepared. Your existing account hasn't changed.

installer-preview-accounts-not-prepared = Connect the accounts you'd like to bring first.

installer-preview-identity-already-prepared = Your identity is already prepared. You can update its name and picture.

installer-preview-identity-not-prepared = Prepare your identity first.

installer-preview-identity-unavailable = Identity preparation isn't available on this system yet.

installer-preview-identity-invalid-color = Choose a valid avatar color.

installer-preview-invalid-choice = That choice isn't available on this page.

installer-preview-wrong-page = This action isn't available on the current page.
installer-demo-unavailable = This action isn't available in the demo.
installer-demo-notice = Preview only · Example data. Nothing will be installed or connected.

installer-preview-at-start = You're at the beginning of setup.

installer-preview-finished = The preview is complete. You can try another path.

installer-preview-avatar-change = Change picture…

installer-preview-avatar-picker = Choose your Tilo picture

installer-preview-avatar-choose = Choose picture

installer-preview-avatar-filter = Pictures

installer-preview-avatar-failed = That picture couldn’t be used. Choose another picture or keep your initials.

installer-preview-avatar-capture = Picture selection is available in the interactive preview.

installer-preview-storage-checking = Checking this computer’s storage…

installer-preview-storage-capacity = { $capacity } disk

installer-preview-storage-size-label = Space for Tilo

installer-preview-storage-size-range = Tilo needs at least { $min } GiB. Up to { $max } GiB is available.

installer-preview-storage-existing = Your current system

installer-preview-storage-tilo = Tilo

installer-preview-storage-temporary = Temporary installer space

installer-preview-storage-shrink = Your current system’s partition will be resized. Its files stay in place.

installer-preview-storage-shortfall = This installation needs { $needed }; { $available } is available. Keep the original files and system intact. Choose a smaller Tilo size if offered, or use a USB installer with another destination disk. If you move files to existing external storage first, independently verify the copy before freeing space. You can exit now without installing.

installer-preview-storage-unreadable = This disk cannot be prepared from your current system. You can use a USB installer instead.
installer-recovery-heading = Copy to a recovery folder
installer-recovery-native-unavailable = Verified resumable recovery copying is unavailable on this host: native directory-entry durability has not been qualified. Keep the original and use the ordinary installation media path; no backup or cloud recovery is assumed.
installer-recovery-retain-source = The original remains authoritative. Remote and cloud placeholders are unverified. A verified copy does not authorize replacing Windows or freeing source space.
installer-recovery-choose-directory = Choose recovery folder
installer-recovery-copy = Copy and verify
installer-recovery-retry = Retry and reverify
installer-recovery-cancel = Stop copying
installer-recovery-not-independent = This folder is not proven to be on independent physical storage. Another partition on the same disk is not an independent backup.
installer-recovery-state-not-selected = Choose an explicit destination folder to review its capacity and durability.
installer-recovery-state-reviewed = Destination reviewed. No data has been copied yet.
installer-recovery-state-copying = Copying and verifying. Keep both storage devices connected.
installer-recovery-state-verified = Local copy verified. Keep the original until installation and recovery requirements are satisfied.
installer-recovery-state-cancelled = Copy stopped. Verified partial output is retained for retry.
installer-recovery-state-incomplete = Some data could not be verified. The original must remain.
installer-recovery-state-interrupted = Copy interrupted. Retry to recheck source identity and verified receipts.
installer-recovery-source-required = Select an exact Windows source folder first.
installer-recovery-review-failed = Recovery destination could not be reviewed. Select a writable private folder with sufficient space and supported durable storage.
installer-recovery-copy-failed = Recovery could not start. Review the destination or retry the interrupted copy.
installer-recovery-copy-required = Finish and verify the selected recovery copy before continuing.
installer-recovery-write-guard = Recovery data cannot be proven outside the installation write ranges. Keep the original and choose another destination.

installer-preview-storage-release-blocked = Direct installation is not available in this build. You can install from USB instead.

installer-preview-storage-unavailable = Direct installation is not available. Use a USB installer instead.

installer-preview-storage-range-error = Choose a size within the available range.

installer-preview-usb-heading = Prepare a USB installer

installer-preview-usb-intro = Use it to start Tilo on your other computer.

installer-preview-desktop-heading = Add Tilo as a desktop

installer-preview-desktop-intro = Keep your existing Linux system.

installer-preview-prepare-usb = Make a USB installer

installer-preview-save = Save changes

installer-preview-cancel = Cancel

installer-preview-folders-checking = Looking for your folders…

installer-preview-folders-none = No standard folders found on this computer.

installer-preview-import-unavailable = Not available in this build.

installer-preview-flatpak-title = Flatpak apps

installer-preview-flatpak-body = Your installed apps, their data and settings.

installer-preview-flatpak-found = { $count } installed apps, with their data and settings.

installer-preview-flatpak-unavailable = Could not read installed Flatpak apps. Try again before transferring.

installer-preview-home-unavailable = Could not locate your home folder.

installer-preview-accounts-none = No supported accounts found.

installer-preview-account-importable = Saved connection can be brought over.

installer-preview-account-tilo = Saved Tilo connection

installer-preview-account-reconnect = Sign in again to connect this service in Tilo.

installer-preview-accounts-incomplete = Some account settings could not be read.

installer-preview-belongings-check-failed = Could not finish checking your belongings. Try again.

installer-preview-transfer-busy = A transfer is already being reviewed or running.

installer-preview-transfer-needs-review = Review the transfer destination first.

installer-preview-transfer-start-failed = Could not start the reviewed transfer.

installer-preview-transfer-not-running = No transfer is running.

installer-preview-account-action-failed = Could not connect this account. Check the account and destination, then try again.

installer-preview-flatpak-system-title = System Flatpak apps

installer-preview-flatpak-system-found = { $count } system-installed apps. Your user apps and app data are already included with your home folder.

installer-preview-flatpak-system-body = Restore apps installed for all users. Your user apps and their data come with your home folder.

installer-accounts-heading = Connect your accounts

installer-accounts-source-label = { $service }: { $account }

installer-accounts-source-identity = { $account } ({ $identity })

installer-accounts-intro = Connect the services you want in Tilo. This is optional; after setup, open Settings → External accounts to connect them.

installer-accounts-add-heading = Add an account

installer-accounts-google-drive = Google Drive

installer-accounts-onedrive = OneDrive

installer-accounts-dropbox = Dropbox

installer-accounts-spotify = Spotify

installer-accounts-add = Connect an account

installer-accounts-saved = Saved connection

installer-accounts-left-out = Left out

installer-accounts-sign-in = Sign in to connect

installer-accounts-connected = Ready to bring over

installer-accounts-import-heading = Bring this connection with you

installer-accounts-import-intro = Use the saved connection in Tilo. Your existing account stays unchanged.

installer-accounts-connect-heading = Connect { $service }

installer-accounts-cloud-intro = Make your cloud files available in Tilo.

installer-accounts-spotify-intro = Find your Spotify library in Tilo.

installer-accounts-files-permission = Approve access to read and update your cloud files in your browser.

installer-accounts-spotify-permission = Allow Tilo to read your saved music.

installer-accounts-import = Bring connection

installer-accounts-connect = Connect

installer-accounts-importing-heading = Bringing your connection over

installer-accounts-importing-intro = This usually takes a moment.

installer-accounts-waiting-heading = Finish signing in

installer-accounts-waiting-intro = Approve access in your browser. This page updates when you're done.

installer-accounts-open-again = Open sign-in again

installer-accounts-cancel = Cancel sign-in

installer-accounts-cancelling-heading = Closing sign-in

installer-accounts-cancelling-intro = Your account won't be added.

installer-accounts-done-heading = Account connected

installer-accounts-done-intro = { $service } is ready for Tilo.

installer-accounts-another = Connect another account

installer-accounts-failed-heading = Couldn't connect this account

installer-accounts-retry = Try again

installer-accounts-skip = Skip for now

installer-accounts-browser-failed = Your browser didn't open. Open sign-in again to continue.

installer-accounts-unavailable = This connection is currently unavailable.

installer-accounts-connect-failed = Sign-in didn't finish. Try again or connect this account later.

installer-accounts-import-failed = This saved connection couldn't be used. Connect the account again later.

installer-accounts-save-failed = Your connection couldn't be saved. Try again.

installer-accounts-remove-failed = Couldn't close this connection. Try again before continuing.

installer-accounts-conflict = This connection already has different settings. Your saved connection wasn't replaced.

installer-accounts-sources-changed = Your account settings changed. Check your belongings again.

installer-accounts-invalid-action = Choose an account to continue.

installer-accounts-profile-only = This app's profile comes with your other data. Connect it after starting Tilo.

installer-accounts-busy = Finish or cancel this sign-in first.

installer-accounts-already-prepared = These connections are already part of your installation.

installer-accounts-continue = Continue

installer-preview-account-handoff-unavailable = Your selected account connections could not be read. Return to accounts and try again.

installer-accounts-nextcloud = Nextcloud

installer-accounts-webdav = WebDAV

installer-accounts-app-password-needed = Add with an app password

installer-accounts-dav-server-heading = Where are your files?

installer-accounts-dav-server-intro = Enter the server address for this account.

installer-accounts-dav-server-label = Server address

installer-accounts-dav-login-heading = Your account on this server

installer-accounts-dav-login-intro = Enter the login name you use for your files.

installer-accounts-dav-login-label = Login name

installer-accounts-dav-password-heading = Add your app password

installer-accounts-dav-password-intro = Use an app password for this service. Your existing account stays unchanged.

installer-accounts-dav-password-label = App password

installer-accounts-dav-save = Save connection

installer-accounts-dav-required = Fill in this detail to continue.

installer-accounts-dav-server-invalid = Enter a valid HTTPS server address.

installer-accounts-dav-saved-heading = Your connection is saved

installer-accounts-dav-saved-intro = Tilo will use these account details when you open your files.

imager-setup-action-use-migration-intent = Use reviewed migration

imager-setup-param-migration-intent = Private directory containing the reviewed migration

imager-setup-error-migration-intent-unavailable = A reviewed migration can be attached on the Full Tilo review before installation starts.

imager-setup-error-migration-intent-invalid = That migration could not be verified. Review your belongings again.

installer-accounts-nextcloud-method-heading = Connect your Nextcloud account

installer-accounts-nextcloud-method-intro = Sign in with your browser or use an app password you already have.

installer-accounts-nextcloud-browser = Sign in with browser

installer-accounts-nextcloud-app-password = Use an app password

installer-accounts-nextcloud-add = Sign in to bring your files

installer-accounts-starting-heading = Preparing your sign-in

installer-accounts-starting-intro = Contacting your server. You can go back or cancel.

installer-migration-home-picker = Allow access to your home folder: { $home }

installer-migration-home-allow = Allow access

installer-migration-prepare-failed = Your belongings couldn’t be prepared for this installation. Your original files have not changed.

installer-migration-preparing = Preparing your belongings…

installer-existing-picker = Choose your identity backup

installer-existing-choose = Choose backup…

installer-existing-file-body = Choose the identity backup you saved from Tilo.

installer-existing-password-label = Backup passphrase

installer-existing-password-required = Enter the passphrase for this backup.

installer-existing-file-required = Choose your identity backup first.

installer-existing-file-invalid = Choose a local identity backup file.

installer-existing-busy = Your identity is being restored.

installer-existing-restoring = Restoring your identity…

installer-existing-connected = Your Tilo identity is ready for this computer.

installer-existing-failed = We couldn’t restore this backup. Check its passphrase or choose another backup.

installer-existing-pairing-unavailable = Connecting from another Tilo device is not available in this installer yet. You can restore an identity backup instead.

installer-media-prepare-failed = Your setup couldn’t be prepared for the USB installer. Your identity and connected accounts are still saved here. Try again.

installer-media-deliver-heading = Take your setup with you

installer-media-deliver-body = The installer image is verified. Your encrypted setup and the belongings you selected still need to be added.

installer-media-delivering = Adding your encrypted setup and selected belongings to the USB…

installer-media-delivery-failed = Your setup couldn’t be added. Keep the USB connected and try again.

installer-media-choose-filesystem = Choose the USB folder

installer-media-filesystem-picker = Allow access to the verified USB installer

installer-media-filesystem-allow = Add my setup

installer-media-delivery-busy = Your setup is being added. Wait for it to finish or cancel.

installer-media-not-prepared = Prepare your identity and accounts first.

installer-media-not-written = Write and verify the installer medium first.

installer-media-already-delivered = Your setup is already on this medium.

installer-media-delivery-not-running = No setup delivery is running.

installer-media-official-required = Choose a Tilo installer image to carry your prepared setup.

installer-media-cancel = Cancel transfer

installer-accounts-icloud = iCloud Drive

installer-accounts-icloud-add = Connect with your Apple Account.

installer-accounts-icloud-login-heading = Connect iCloud Drive

installer-accounts-icloud-login-intro = Use the Apple Account you use for iCloud.

installer-accounts-icloud-login-label = Apple Account

installer-accounts-icloud-password-heading = Sign in to iCloud

installer-accounts-icloud-password-intro = Your password is used to sign in and is not saved in Tilo.

installer-accounts-icloud-password-label = Apple Account password

installer-accounts-icloud-code-heading = Check your trusted device

installer-accounts-icloud-code-intro = Enter the verification code Apple sent to your trusted devices.

installer-accounts-icloud-code-label = Verification code

installer-accounts-icloud-wait-heading = Connecting iCloud Drive…

installer-accounts-icloud-wait-intro = Waiting for Apple to confirm your sign-in.

installer-accounts-icloud-required = Fill in this field to continue.

installer-accounts-icloud-login-invalid = Enter your Apple Account email address or phone number.

installer-accounts-icloud-credentials-invalid = Your Apple Account or password was not accepted. Try again.

installer-accounts-icloud-code-invalid = That code was not accepted. Enter a new code from your trusted device.

installer-accounts-icloud-web-access = Allow access to iCloud data on the web in iCloud settings on your trusted device, then try again.

installer-accounts-icloud-locked = Your Apple Account needs attention. Review it at account.apple.com, then try again.

installer-accounts-icloud-offline = Apple could not be reached. Check your connection and try again.

installer-media-retry = Try again

installer-media-ready = Your installer and setup are verified. Eject the USB before removing it.

installer-accounts-remove-heading = Leave { $service } out of this setup?

installer-accounts-remove-intro = This connection will be removed from your Tilo setup. Your original account and files stay unchanged.

installer-accounts-remove-confirm = Leave it out

installer-accounts-remove-keep = Keep this connection

installer-accounts-included = Included in this setup

installer-accounts-import-done-heading = Your saved connection is ready

installer-accounts-import-done-intro = This saved connection is included in your Tilo setup.

installer-portable-source-busy = Your belongings are being reviewed.

installer-portable-source-failed = Your home folder could not be checked. Your files and setup choices are kept. Try checking again.

installer-portable-permission-failed = Access to this home folder was not available. Allow access to the home folder you reviewed, then try again.

installer-portable-metadata-unsupported = Some file permissions or metadata cannot be transferred safely yet. Nothing has been left out or copied. Your setup choices are kept.

installer-portable-mounted-source-unsupported = Your home includes mounted files or folders. Bringing the whole home across these locations is not supported yet. Nothing has been copied, and your setup choices are kept. Use “Choose what comes over” to change your selection. Continuing without Home will not transfer your full home.

installer-mounted-home-locations = Mounted locations in your home

installer-mounted-home-location-unavailable = This location cannot be transferred as part of your full home yet.

installer-portable-applications-review-failed = Your selected apps and their data could not be checked. Check your belongings again before continuing.

installer-portable-home-required = Bringing only system Flatpak apps to another computer is not available yet. Choose your home folder to bring your user apps and their data.

installer-preview-belongings-shared-intro = Choose what to use in Tilo. Your files stay where they are.

installer-preview-files-shared-title = Your files

installer-preview-files-shared-body = Use the same files in both systems. Edits appear in both.

installer-preview-migrate-shared-body = Use the same files in both systems; edits appear in both. Keep the disk partition holding your originals. Sharing is not a backup, and apps and setup still need extra space.

installer-preview-review-shared = Use your existing files in both systems. Edits appear in both. Bring the apps and accounts you selected.

installer-preview-account-profile-only = App or browser profile found. This installer cannot connect its existing login to Tilo.

imager-setup-internal-error-prepared-setup = Your prepared identity and accounts could not be read. Return to setup and try again.

imager-setup-internal-error-guided-navigation = Use Back in the installer to change your setup.

installer-preview-storage-app-estimate = Estimated space for your apps and their data: { $size }. Includes room for installation and updates.

installer-preview-prepare-direct = Review installation

installer-preview-install-profile-invalid = Your installation preferences need to be corrected before continuing.

installer-preview-direct-review-unavailable = Installation could not be prepared for this computer. Your setup has been kept; you can try again or make a USB installer.

installer-preview-install-preparing = Preparing your identity and accounts for this installation…

installer-preview-device-invalid = Choose a shorter device name using a single line.

installer-accounts-code-heading = Enter the code from Dropbox

installer-accounts-code-intro = Finish signing in in your browser, then paste the code Dropbox gives you here.

installer-accounts-code-label = Sign-in code

installer-accounts-code-submit = Connect Dropbox

installer-accounts-code-invalid = Paste the code from Dropbox before continuing.

installer-accounts-tilo-saved = Saved Tilo connection. Review before bringing it over.

installer-accounts-tilo-import-intro = Bring this saved Tilo connection into your new setup. If it cannot be unlocked here, you can connect the service again.

installer-accounts-tilo-reconnect = This saved connection could not be unlocked. Choose a service to connect again; your original connection stays unchanged.

installer-accounts-tilo-changed = This saved connection has changed since you reviewed it. Check your belongings again before bringing it over.

installer-accounts-already-included = This connection is already included in your setup.

installer-pairing-body = Open Tilo on your other device to approve this computer.

installer-pairing-start = Connect to my Tilo device

installer-pairing-waiting = Check that your other device shows the same code before approving.

installer-pairing-cancel = Cancel pairing

installer-pairing-cancelled = Pairing stopped here. If you already approved this device, it may still appear on your other device.

installer-pairing-failed = We couldn’t connect this device. Try again or use your identity backup.

installer-pairing-busy = Pairing is already in progress.

installer-pairing-helper-unavailable = This installer cannot connect to another Tilo device. Use your identity backup or a supported native Linux installer.

installer-pairing-label-locked = This device name was approved by your other Tilo device and cannot be changed here.

installer-pairing-name-body = Choose the name your other Tilo device will see when you connect this computer.

installer-pairing-name-heading = What will you call this computer?

installer-pairing-not-running = Pairing is not running on this computer.

installer-pairing-code-intro = Open Tilo on your other device. Check that it shows this same code before approving this computer.

installer-pairing-code-pending = Getting a code to connect this computer…

installer-restart-heading = Welcome back

installer-restart-intro = Your unfinished preparation is still here. Would you like to continue?

installer-restart-checking = Checking your saved preparation…

installer-restart-incomplete = Your previous preparation did not finish. Continue to check whether its identity can be recovered, or start again.

installer-restart-unavailable = We couldn't verify your saved preparation. Your saved identity files have been kept. Retry to check again.

installer-restart-resuming = Checking your identity…

installer-restart-retiring = Keeping your previous identity files and starting again…

installer-restart-resume = Continue preparation

installer-restart-new = Start again

installer-restart-retry = Retry

installer-restart-busy = Your saved preparation is being checked. Please wait.

installer-restart-unlock-heading = Unlock your preparation

installer-restart-unlock-intro = Enter the password you chose earlier to recover your prepared accounts.

installer-restart-password-required = Enter the password you chose earlier.

installer-restart-changed = Review the latest saved choices and accounts before continuing.

installer-restart-changed-heading = Your setup has changed

installer-restart-review-latest = Review latest setup

installer-accounts-checkpoint-unlock-failed = Couldn’t unlock your saved connections. Check your password and try again.

installer-accounts-provider-heading = Which account would you like to connect?

installer-accounts-provider-intro = Choose a service to sign in.

installer-accounts-spotify-done-heading = Your Spotify library is connected

installer-accounts-spotify-done-intro = Your saved music is ready for Tilo.

installer-preview-files-location = Your home folder: { $path }

installer-preview-belongings-incomplete = Some belongings couldn’t be checked. You can check again, or continue with the available choices.

installer-preview-storage-shared-files = Your documents stay on your current system’s partition and are shared with Tilo. This space is for Tilo, its updates, and separate copies of your apps’ data and settings.

installer-preview-profiles-title = App and browser profiles

installer-preview-profiles-body = Profile files stay with your other files. Connecting services in Tilo is a separate choice.

installer-preview-storage-show-details = Disk details

installer-preview-storage-usb-fallback-heading = Install Tilo from USB

installer-preview-storage-usb-fallback-intro = Direct installation is not available in this build. Use a USB drive to install Tilo on this computer.

installer-accounts-spotify-library = Spotify library

installer-accounts-spotify-playback = Spotify playback

installer-accounts-spotify-playback-intro = Connect Spotify to play your music in Tilo.

installer-accounts-spotify-playback-permission = Allow Tilo to stream music with your Spotify account. Playback requires Spotify Premium.

installer-accounts-spotify-playback-done-heading = Spotify playback is connected

installer-accounts-spotify-playback-done-intro = You can play Spotify music after starting Tilo.

installer-accounts-spotify-purpose-heading = What would you like to connect?

installer-accounts-spotify-purpose-intro = Choose how you’ll use Spotify in Tilo.

installer-accounts-spotify-library-choice = Your saved music

installer-accounts-spotify-library-choice-intro = Find your Spotify library in Tilo.

installer-accounts-spotify-playback-choice = Listen to music

installer-accounts-spotify-playback-choice-intro = Play Spotify music in Tilo. Requires Spotify Premium.

installer-accounts-spotify-library-saved = Saved Tilo library connection. Review before bringing it over.

installer-accounts-spotify-playback-saved = Saved Tilo playback connection. Review before bringing it over.

installer-browser-title = Your browsing

installer-browser-bookmarks = Bookmarks

installer-browser-history = Browsing history

installer-browser-body = Bring your bookmarks and browsing history into Tilo’s browser.

installer-browser-bookmarks-body = Bring your bookmarks into Tilo’s browser.

installer-browser-history-body = Bring your browsing history into Tilo’s browser.

installer-desktop-prepare = Set up Tilo Desktop

installer-desktop-password-intro = Choose a password to protect your prepared Tilo identity and connections. Your computer’s login password stays the same.

installer-desktop-keep-existing = Your files, applications and Linux account stay unchanged. Your current desktop remains the default session.
installer-desktop-unavailable = Tilo Desktop setup is unavailable on this computer.

installer-desktop-enrolled-unavailable = This prepared device identity is for a new Tilo device. To use Tilo Desktop on this computer, use an identity backup.

installer-desktop-delivering = Preparing your first Tilo session…

installer-desktop-delivery-failed = Your Tilo session could not be prepared. Try again before signing in to Tilo.

installer-desktop-ready = Tilo Desktop is ready. Sign out when you’re ready, then choose Tilo at login.

installer-desktop-retry = Try again

installer-desktop-resource = Tilo Desktop installation

installer-desktop-install = Install Tilo Desktop

installer-desktop-cancel = Cancel download

installer-accounts-nextcloud-source-reconnect = Your saved Nextcloud sign-in could not be accessed. Connect this account again to bring it into Tilo.

installer-accounts-reconnect = Connect again

installer-accounts-import-saved = Bring my saved connections

installer-accounts-review-saved = Choose what comes over

installer-accounts-review-saved-heading = Your saved connections

installer-accounts-review-saved-intro = Bring these connections into Tilo. Uncheck any you would rather leave out. Your existing sign-ins stay unchanged.

installer-accounts-saved-summary =
    { $count ->
        [one] One saved connection found.
       *[other] { $count } saved connections found.
    }

installer-accounts-saved-attention = Not included yet. Review how to connect this account.

installer-accounts-skip-saved = Leave saved connections out

installer-accounts-continue-without = Continue without accounts

installer-accounts-continue-included = Continue with included accounts

installer-accounts-saved-limit = Choose up to 64 connections to bring over.

installer-accounts-bulk-heading = Bringing your saved connections

installer-accounts-bulk-intro = Your existing sign-ins stay unchanged. You can leave out any connections that need attention.

installer-accounts-bulk-working = Preparing the next connection…

installer-source-changed = Your files changed while you were preparing Tilo. Review them again to include the latest version. Your identity and connected accounts are kept.

installer-source-review = Review changed files

installer-media-capacity-checking = Checking the image size before choosing a drive.

installer-media-capacity-not-ready = The installation image size is not available yet. Go back and select a ready image, then try again.

installer-media-capacity-invalid = The space needed for this installation could not be calculated. Go back and review the image and migration choices.

installer-media-capacity-estimate = Allow at least { $required } for the installer and everything you selected. This estimate includes space for filesystem metadata.

installer-media-capacity-required = Estimated space needed: { $required }. This drive has { $available }. Choose a drive with enough space.

installer-media-source-checking = Checking your selected files before writing the drive.

installer-media-source-failed = Your selected files could not be verified. The drive has not been written. Try again or return to your preparation.

installer-media-plan-changed = Your image or drive selection changed during review. Review the plan again before writing.

installer-accounts-rclone-source-reconnect = This saved connection could not be used. Sign in again to connect it to Tilo; your original connection stays unchanged.

imager-setup-account-consumer-unavailable = This Tilo release cannot use your saved connections. Choose a newer release, or go back and leave these connections out.

installer-source-unavailable = Your prepared files are no longer available. Review them again to prepare the latest version. Your identity and connected accounts are kept.

installer-source-refresh = Review files again

installer-belongings-files-unchecked = Your home folder couldn’t be found. Check again to include your files, or continue without them.

installer-belongings-apps-unchecked = Your installed Flatpak apps couldn’t be checked. Check again to include them, or continue without them.

installer-belongings-browser-unchecked = Your browser bookmarks and history couldn’t be checked. Check again to include them, or continue without them.

installer-belongings-accounts-unchecked = Some saved connections couldn’t be read. Check again, or continue and connect those accounts later.

imager-setup-home-consumer-unavailable = This Tilo release cannot bring your files and app data over. Choose a newer release before continuing.

installer-service-sessions-title = Website sign-ins

installer-service-sessions-body = Bring your Slack or Discord sign-ins from a browser you use. You may still need to sign in again.

installer-review-service-session-body = You may need to sign in again.

installer-service-source-slack = Slack — { $source }

installer-service-source-discord = Discord — { $source }

installer-service-source-changed = This browser source changed. Choose it again.

installer-service-sources-unavailable = Browser sign-ins couldn’t be checked. You can connect to Slack or Discord after setting up Tilo.

installer-service-session-reconnect = This browser sign-in couldn’t be brought over. Choose another browser source, or connect after setting up Tilo.

installer-service-source-discord-origin = Discord app session — { $source }

installer-media-not-preparing = Preparation is not running.

installer-media-preparation-cancelled = Preparation stopped. Your choices are kept; you can try again.

installer-media-phase-review = Checking your selected files

installer-media-phase-filesystem = Preparing the drive for your files

installer-media-phase-applications = Preparing your applications and their data

installer-media-phase-browser = Preparing your browsing data

installer-media-phase-identity = Preparing your identity

installer-media-phase-accounts = Preparing your selected accounts

installer-media-phase-setup = Encrypting your setup

installer-media-phase-verify = Verifying your encrypted setup

installer-media-phase-home = Encrypting and verifying your home files

installer-media-phase-publish = Saving and verifying your setup on the drive

installer-media-phase-cancelling = Stopping safely…

installer-media-phase-archive = Encrypting your files

installer-media-phase-archive-verify = Checking the encrypted files

installer-media-phase-source-verify = Checking the original files

installer-media-phase-ciphertext = Delivering the encrypted files

installer-media-progress-bytes = { $size } processed

installer-media-progress-bytes-total = { $size } of { $total } processed

installer-accounts-goa-source-reconnect = This saved online account could not be brought over. Sign in again to connect it to Tilo.

installer-migration-grant-failed = Allow access to the home folder shown in the request, then try again. Your setup choices are kept.

installer-migration-source-replaced = The home folder you approved is no longer the same folder. Check your belongings and approve the recognized folder again. Your setup choices are kept.

installer-migration-source-unsupported = This source disk or filesystem cannot be shared safely yet. Your files and setup choices are kept; nothing has been installed.

installer-migration-state-unavailable = Tilo could not save your setup privately on this computer. Your original files have not changed.

installer-migration-state-full = There is not enough space to save your setup on this computer. Free some space, then try again. Your original files have not changed.

# Writer ownership and safe-close lifecycle.

imager-write-stopping = Stopping safely… Keep the drive connected until cancellation finishes.

imager-write-exit-unowned = The image writer's exit cannot be verified.

imager-write-worker-stopped = The image writer stopped unexpectedly.

imager-write-worker-start-failed = Could not start the image writer: { $error }

installer-media-worker-stopped = Your setup transfer stopped unexpectedly.

installer-media-cleanup-failed = Your setup transfer has not finished closing.

installer-service-source-needs-access = Encrypted website data was found in { $source }. Choose it to try reading it with your current computer’s keychain. You may need to sign in again in Tilo.

installer-service-slack-label = Slack

installer-service-discord-label = Discord

installer-service-sessions-overview = Selected: { $services }.

installer-service-sessions-none-selected = No website sign-ins selected.

installer-service-sessions-selected-needs-access = Permission is needed to read selected browser data for { $services }.

installer-service-sessions-accounts-excluded = Website data is not included because Accounts is off. Turn Accounts on to restore your choices.

installer-review-setup = Your setup

installer-review-device = Device: { $device }

installer-review-person-body = Your Tilo identity

installer-review-device-body = This device in Tilo

installer-review-device-enrolled = Approved on your existing Tilo device

installer-review-belongings-heading = Your belongings

installer-review-belongings = Bringing with you

installer-review-start-fresh = Starting fresh

installer-original-grant-required = Review the files you want to bring from this computer first.

installer-original-permission-progress = Preparing access to your original files

installer-original-prepare-failed = Your files could not be prepared. Nothing was written to the installation drive. Review your choices and try again.

installer-original-output-unavailable = This installation drive cannot receive your prepared files yet. Nothing has been written to it.

installer-original-preparing = Preparing…

installer-original-retry = Try again

installer-original-preparing-heading = Getting your files ready

installer-original-preparing-body = Your files stay on this computer. We’re preparing the installation drive to receive them when you restart.

installer-original-not-prepared = Your files are not ready for this installation drive yet. Try preparing them again.

installer-original-boot-heading = Restart this computer from the Tilo drive

installer-original-boot-body = Your setup is saved on the drive. Your files are still on this computer.

installer-original-boot-step = Keep { $drive } connected. Restart this computer and choose it in the boot menu. Tilo will copy your selected files before you take the drive to your other computer.

installer-original-boot-drive = the Tilo installation drive

installer-original-ready-heading = Prepare your files for this drive

installer-original-ready-body = We’ll prepare the installation drive to receive your files when you restart. Your files stay on this computer.

installer-original-failed-heading = Your files aren’t ready yet

installer-original-failed-body = Your files are still on this computer.

installer-original-prepare-button = Prepare files

installer-original-complete-progress = Bringing your files into Tilo

installer-original-complete-failed = Your migration could not be completed. Your original files are unchanged.

installer-identity-route-unavailable = This way of connecting your Tilo identity isn’t available for this installation.

installer-review-usb-fallback = Installation on this computer needs a USB drive. Make the installer, then restart this computer from it.

installer-review-usb-choice-required = Choose Make a USB installer to continue on this computer.

installer-review-installation-unavailable = Installation is unavailable here. Your setup choices are kept.

installer-flatpak-settings-note = These apps have custom access settings. You may need to set them again after moving.

installer-flatpak-settings-unchecked = Custom app access settings couldn’t be checked.

installer-flatpak-settings-folders = Folders

installer-flatpak-settings-network = Connections

installer-flatpak-settings-devices = Devices, sound and display

installer-flatpak-settings-services = System services

installer-flatpak-settings-environment = App settings

installer-flatpak-settings-other = Other access settings

installer-flatpak-settings-title = Custom app access

installer-storage-check-capacity = Check available space…

installer-storage-btrfs-resize = Your current system will keep { $after } of its { $before }. We keep at least { $reserve } above the filesystem’s required space. Your files stay in place.

installer-files-source-home = From your home folder

installer-files-show-location = Show location

installer-files-hide-location = Hide location

installer-mounted-home-grant-intro = Some files and folders in your home need separate permission. Allow access to each item shown here.

installer-mounted-home-picker-title = Allow access to the file or folder shown in setup

installer-mounted-home-grant = Allow access

installer-mounted-home-granted = Access allowed

installer-mounted-home-needs-grant = Needs permission

installer-mounted-home-grants-required = Allow access to every listed item before continuing.

installer-mounted-home-review = Continue

installer-mounted-home-cancel = Cancel

installer-mounted-home-restart = Review these files and folders again

installer-mounted-home-all-granted = Access is allowed for all listed files and folders. Continue with your setup.

installer-mounted-home-permission-heading = Allow access

installer-original-image-unavailable = This installer image cannot bring your data over. Choose a newer Tilo installer.

installer-flatpak-apps-heading = Your apps

installer-flatpak-app-with-home = Included with your home folder

installer-flatpak-app-selected = Included with your selected apps

installer-flatpak-apps-existing-heading = Your installed apps

installer-flatpak-app-existing = Already on this computer

installer-flatpak-app-excluded = Not included

installer-guided-host-unavailable = Prepare Tilo from a Linux computer. Installation setup isn’t available on this computer yet.

installer-flatpak-app-needs-home = Bring your home folder to include this user-installed app and its settings.

installer-flatpak-app-transfer-unavailable = This user-installed app is not transferred by this installation. Your original app and its settings stay in place.

imager-setup-applications-consumer-unavailable = This Tilo release cannot bring your selected apps and their settings over. Choose a newer release before continuing.

installer-preview-current-title = Use this computer’s identity

installer-preview-current-body = Continue with the Tilo identity already used on this computer.

installer-current-identity-unavailable = This computer’s identity is unavailable. Choose another way to connect.

installer-current-device-name-locked = Keep this computer’s existing device name.

installer-review-device-current = This computer’s existing name

installer-desktop-belongings-intro = Your files and apps stay on this computer. Choose browser data and connections to use in Tilo.

installer-desktop-files-in-place = Your files stay where they are.

installer-desktop-apps-in-place = Your installed apps and their settings stay in place.

installer-desktop-belongings-in-place = Your files and apps stay in place for Tilo Desktop. Choose browser data or account connections instead.

installer-desktop-review-migrate = Keep your existing files and apps in place. Bring the browser data and connections you selected.

installer-service-sessions-found-attention = Found sign-in data for { $services }.

installer-service-source-reconnect-required = Encrypted sign-in data was found in { $source }. It cannot be copied yet. Sign in to Slack again in Tilo.

installer-service-reconnect-required = This encrypted Slack sign-in cannot be copied yet. Sign in to Slack again in Tilo.

installer-accounts-spotify-source-sign-in = Sign in to connect your Spotify library or playback.

installer-accounts-spotify-source-reconnect = Sign in to use Spotify in Tilo. Your existing Spotify session won’t be moved.

installer-accounts-dav-auth-refused = The server rejected these sign-in details.

installer-accounts-dav-endpoint-unavailable = Couldn’t reach the server. Try again.

installer-accounts-dav-files-unavailable = The server didn’t provide access to this files folder.

# Folder review and copy-time eligibility share these explanations.
imager-folder-cloud-unavailable = Windows marks this item as offline or requiring recall. Its contents are not counted or copied, and Tilo will not download them. Make it available locally in Windows and review again; this may need more disk space.
imager-folder-reparse-unsupported = This item is a Windows reparse point, such as a link, junction or cloud-managed item. This copier does not support it, even if its contents are local. Its contents are not counted or copied.

# Same-computer app state is copied separately from shared original documents.

installer-preview-flatpak-computer-found = { $count } apps available to copy, with their selected data and settings.

installer-preview-flatpak-computer-body = Copy selected apps, their data and settings into Tilo. Later app changes stay in Tilo; edits to shared files appear in both systems.

# Native Linux Nextcloud discovery reads account identity, not client sync status.
installer-accounts-nextcloud-linux-summary = Found a Linux Nextcloud account, not a verified backup. Review local changes before erasing Linux.
installer-accounts-nextcloud-linux-files = Your existing Linux sync folders stay unchanged. Tilo accesses files on the Nextcloud server on demand; it does not turn your Linux sync folder into a cloud mount.
installer-accounts-nextcloud-linux-local-changes = Account discovery does not check pending uploads, folder mappings, exclusions or conflicts. Local-only changes may not be on the server. Before erasing Linux, let the Nextcloud client finish syncing and resolve conflicts, or copy local-only changes somewhere safe.
installer-accounts-nextcloud-linux-dual-boot = Keeping Linux? Edits and deletions that sync to the Nextcloud server affect the files you access from both systems. Choose separately which local folders to copy or share with Tilo.
installer-accounts-nextcloud-linux-calendar = Connecting Nextcloud files does not configure Calendar. After setup, use Connect calendars for this account in Settings to choose calendars.
