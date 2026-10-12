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
