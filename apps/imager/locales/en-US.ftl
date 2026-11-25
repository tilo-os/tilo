
# Image write review. The remaining image contents are written unchanged.
imager-review-setup-hostname = Written with hostname { $hostname }, then verified

# The write review's rows, and the window's own status and result copy.
imager-review-image = Image
imager-review-image-size = Image size
imager-review-drive = Drive
imager-review-device = Device
imager-review-space = Space
imager-review-setup = Setup
imager-review-none = None selected
imager-review-preconfigured = Pre-configured for { $user } on { $hostname }
imager-review-unchanged = Written unchanged, then verified
imager-rescan-failed = Drive rescan failed. Check the system disk tools and try again.
imager-rescan-summary = Rescanned — { $removable } removable, { $total } total
imager-rescan-done = Rescanned drives
imager-done-image = The image
imager-done-drive = the drive
imager-done-installed-title = Tilo is installed
imager-done-installed-body = { $image } was written to { $drive } and verified. Remove the installation media and restart.
imager-done-ready-title = The drive is ready
imager-done-ready-eject = { $image } was written to { $drive } and verified. Eject it before you take it out.
imager-done-ready-remove = { $image } was written to { $drive } and verified. You can safely remove the drive.
imager-done-install-failed = The installation failed
imager-done-write-failed = Writing failed
imager-done-incomplete = The drive may be incomplete. Review the plan before trying again.

# Guided setup semantic resource and action labels.
imager-setup-action-next = Continue setup
imager-setup-action-start-download = Download Tilo
imager-setup-action-pause-download = Pause download
imager-setup-action-resume-download = Resume download
imager-setup-action-cancel-download = Cancel download
imager-setup-action-discard-download = Discard unfinished download data
imager-setup-action-release-download = Keep download for later

imager-setup-error-download-start-unavailable = A Full Tilo download cannot start in its current state.
imager-setup-error-download-unavailable = Full Tilo download is unavailable.
imager-setup-error-download-review-only = Start the download from the Full Tilo review.
imager-setup-error-download-pause-unavailable = The download is not currently active
imager-setup-error-download-resume-unavailable = This saved download cannot be resumed yet
imager-setup-error-download-cancel-unavailable = The download is not currently active
imager-setup-error-download-discard-unavailable = Unfinished download data is not available to discard
imager-setup-error-download-release-unavailable = The verified download is not available to keep for later
imager-setup-error-download-no-folder = Tilo cannot find the folder it keeps downloads in on this computer.
imager-setup-error-download-unreadable = The saved Tilo download cannot be read.
imager-setup-error-download-unsupported = Full Tilo cannot be downloaded on this computer.
imager-setup-error-download-changed = This download changed since you chose it. Review the current one before continuing.
imager-setup-error-download-selection-too-large = The saved download selection is larger than this computer keeps.
imager-setup-error-download-selection-invalid = The saved download selection is not one Tilo can use.
imager-setup-error-download-lost = The verified download is no longer on this computer.
imager-setup-error-download-unsaved = The download selection could not be saved.
imager-setup-error-staging-unavailable = Protected staging is unavailable.
imager-setup-error-staging-not-built = Protected staging is not part of this build.
imager-setup-error-staging-not-built-retained = Protected staging is not part of this build; the verified download was kept.

imager-setup-resource-root = Guided Tilo setup
imager-setup-badge-review = Review

# Full Tilo installed alongside the current system, without a USB drive.
imager-setup-resource-internal = Full Tilo installation
imager-setup-badge-internal = Full Tilo
imager-setup-detail-internal = { $readiness }
imager-setup-action-internal-set-target-size = Set Tilo size
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
imager-setup-check-title = Check this computer
imager-setup-check-only = Only compatibility checks are available here. Continue to choose what to bring.
imager-setup-check-not-ready = Resolve the compatibility checks before continuing.
imager-setup-check-not-enabled = Direct installation isn’t enabled in this build. Go back to prepare a USB drive instead.
imager-setup-internal-not-enabled = Direct installation isn’t enabled in this build.
imager-setup-internal-no-release = This build has no installer release for this computer’s processor.
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
imager-setup-internal-plan-free = Free, unpartitioned space on this disk holds Tilo. No existing partition is resized.
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

# Guided installer.
installer-preview-destination-heading = Welcome to Tilo

installer-preview-destination-intro = Where would you like to install it?

installer-preview-computer-title = On this computer

installer-preview-computer-body = Install Tilo alongside your current system. You’ll review the disk changes before installing.

installer-preview-computer-usb-body = Direct installation isn’t enabled in this build. You can make a USB installer instead.
installer-native-full-os-body = Install alongside Windows without a USB drive. You’ll review the disk changes before installing.

installer-preview-another-title = On another computer

installer-preview-another-body = Make an install plan and carry it to that computer. Nothing is written on that computer unless you approve it there or on your devices.

installer-preview-migrate-heading = Bring your things over

installer-preview-migrate-intro = Sign in with a ticket from one of your devices, then open Migrations to choose what comes over. You can also do this after setup.

installer-preview-ticket-label = Ticket

installer-migrate-sign-in = Sign in

installer-migrate-open = Open migrations

installer-migrate-signing-in = Signing in…

installer-migrate-signed-in = Signed in.

installer-migrate-sign-in-failed = Couldn’t sign in.

installer-migrate-sign-in-refused = Sign-in refused: { $reason }

installer-migrate-open-failed = Couldn’t open Migrations.

installer-migrate-busy = Wait until sign-in finishes.

installer-migrate-ticket-required = Enter the ticket from your device.

installer-preview-target-heading = Which computer will run Tilo?

installer-preview-target-intro = Name that computer or import the hardware report it produced. The installer on that computer checks its disks, startup and protection before anything changes.

installer-preview-target-identity-heading = The other computer

installer-preview-target-manufacturer-label = Manufacturer

installer-preview-target-model-label = Model

installer-preview-target-variant-label = Variant or board version

installer-preview-target-board-label = Board (optional)

installer-preview-target-sku-label = SKU (optional)

installer-preview-target-architecture-label = Architecture

installer-preview-target-report-architecture-fixed = That report was made on another architecture. Import a report for the architecture you want to prepare, or name the model instead.

installer-preview-target-payload-missing = No verified Tilo image is published for that computer’s architecture, so this computer cannot write a medium that starts there.

installer-preview-target-required = Enter this to identify the other computer.

installer-preview-target-import = Import hardware report…

installer-preview-target-use-model = Name a model instead

installer-preview-target-recheck = Check again

installer-preview-target-scan-hint = To import a report, run the hardware check on the other computer and bring the file it saves.

installer-preview-target-report-invalid = That file is not a Tilo hardware report for another computer.

installer-preview-target-report-unreadable = That file could not be read.

installer-preview-target-report-missing-architecture = The report does not say which architecture that computer uses. Run the hardware check again on it with this version of Tilo.

installer-preview-target-report-unsupported-architecture = Tilo has no build for the architecture named in that report.

installer-preview-target-architecture-unsupported-processor-size = Tilo needs a 64-bit processor with at least 4 GiB of memory, so it cannot run on a 32-bit computer. Choose a 64-bit computer.

installer-preview-target-architecture-unsupported-byte-order = Tilo stores and shares its documents in a little-endian format, so it cannot run on this big-endian computer. Choose a little-endian computer.

installer-preview-target-architecture-unsupported-graphics = No graphics driver for Vulkan compute exists for this computer’s processor, so it cannot draw the desktop. Choose a computer with a graphics card that supports Vulkan compute.

installer-preview-target-busy = Finish choosing a report before choosing another.

installer-preview-target-incomplete = Name a specific model or import a report from the other computer.

installer-preview-target-incomplete-detail = A manufacturer and model, plus a variant, board or SKU, are needed, or a report from the other computer.

installer-preview-target-blocking = A component needed for the desktop is unsupported on that computer. Resolve it there, import a new report, or name the model and continue without checked hardware.

installer-preview-target-review-named = { $model }. Its drives, startup and protection are checked when it installs, before anything changes.

installer-preview-target-review-nameless = Not named. Its drives, startup and protection are checked when it installs, before anything changes.

installer-preview-target-review-imported = { $model }: { $supported } supported, { $limited } limited, { $unsupported } unsupported, { $unverified } not verified.

installer-preview-target-review-imported-unnamed = Hardware report from that computer: { $supported } supported, { $limited } limited, { $unsupported } unsupported, { $unverified } not verified.

installer-preview-target-review-blocking = A component needed for the desktop is unsupported on that computer.

installer-preview-target-check-heading = Checked on the other computer

installer-preview-target-check-boot = Startup and boot

installer-preview-target-check-boot-body = Check before anything changes on that computer.

installer-preview-target-check-storage = Disk space and partitions

installer-preview-target-check-storage-body = Check before anything changes on that computer.

installer-preview-target-check-protection = Protection for your data

installer-preview-target-check-protection-body = Check before anything changes on that computer.

installer-preview-target-check-note = Nothing here changes the other computer. No per-model support result is used; its own hardware report and its own check provide the evidence.

installer-preview-target-file-title = Choose a hardware report

installer-preview-target-file-accept = Import

installer-preview-target-file-filter = Hardware reports

installer-hardware-checking = Checking this computer’s hardware…

installer-hardware-this-computer = This computer

installer-hardware-other-computer = Other computer

installer-hardware-unknown-model = Unknown model

installer-hardware-model-with-variant = { $model } ({ $variant })

installer-hardware-variant = Variant

installer-hardware-board = Board

installer-hardware-sku = SKU

installer-hardware-firmware = Firmware

installer-hardware-running = Running

installer-hardware-architecture = Architecture

installer-hardware-summary = { $supported } supported, { $limited } limited, { $unsupported } unsupported and { $unverified } not verified.

installer-hardware-summary-blocking = { $supported } supported, { $limited } limited, { $unsupported } unsupported and { $unverified } not verified. A component needed for the desktop is unsupported, so installation cannot continue until it is resolved.

installer-hardware-summary-declared = A model name does not show whether Tilo runs on that computer. Name a manufacturer, model and variant or board, or import the hardware report produced on it.

installer-hardware-summary-imported-check = The installer on that computer checks startup, disk changes and protection again before acting.

installer-hardware-summary-observation = These devices are what the running system uses. The selected Tilo release has not been checked against them.

installer-hardware-scope-observation = Observed in the running system, not checked against the Tilo release.

installer-hardware-needs-attention = Needs attention

installer-hardware-working = Working

installer-hardware-not-detected = Not detected

installer-hardware-state-supported = Supported

installer-hardware-state-limited = Limited

installer-hardware-state-unsupported = Unsupported

installer-hardware-state-unverified = Not verified

installer-hardware-state-not-detected = Not detected

installer-hardware-component-graphics = Graphics

installer-hardware-component-display = Displays

installer-hardware-component-wifi = Wi-Fi

installer-hardware-component-ethernet = Ethernet

installer-hardware-component-bluetooth = Bluetooth

installer-hardware-component-audio = Sound output

installer-hardware-component-microphone = Microphone

installer-hardware-component-camera = Camera

installer-hardware-component-fingerprint = Fingerprint reader

installer-hardware-component-power = Power

installer-hardware-component-acpi = ACPI

installer-hardware-component-boot = Startup and firmware

installer-hardware-component-storage = Storage

installer-hardware-component-input = Keyboard and pointing devices

installer-hardware-reason-driver-bound-one = { $device } uses { $driver } in the running system.

installer-hardware-reason-driver-bound-many = { $count } devices use a driver in the running system.

installer-hardware-reason-hybrid-graphics = { $device } uses { $driver }; more than one graphics adapter is present.

installer-hardware-reason-driver-missing = { $count } devices have no driver bound in the running system.

installer-hardware-reason-virtual-adapter = Only { $device } is present; it is a virtual display adapter.

installer-hardware-reason-open-nvidia = { $device } uses the open-source Nvidia driver.

installer-hardware-reason-platform-pipeline = { $device } uses a camera pipeline that cannot be verified here.

installer-hardware-reason-not-detected = No such hardware was found.

installer-hardware-reason-display-disconnected = No display is connected.

installer-hardware-reason-display-inactive = { $device } is connected but not active.

installer-hardware-reason-displays-active = { $internal } built-in and { $external } external displays are active.

installer-hardware-reason-no-playback = { $device } has no playback device.

installer-hardware-reason-no-capture = { $device } has no capture device.

installer-hardware-reason-fingerprint-unverified = { $device } needs a reader-specific driver that cannot be verified here.

installer-hardware-reason-suspend-unavailable = The running kernel does not advertise suspend.

installer-hardware-reason-battery = { $device } is the battery.

installer-hardware-reason-battery-capacity = { $capacity }% charged.

installer-hardware-reason-battery-status = Reported state: { $status }.

installer-hardware-reason-battery-health = { $health }% of its original capacity.

installer-hardware-reason-ac-only = This computer uses mains power and has no battery.

installer-hardware-reason-acpi-tables = { $tables } ACPI firmware tables are available.

installer-hardware-reason-uefi = UEFI startup is available.

installer-hardware-reason-secure-boot-on = Secure Boot is on.

installer-hardware-reason-secure-boot-off = Secure Boot is off.

installer-hardware-reason-tpm = { $tpm } is present.

installer-hardware-reason-legacy-boot = This computer did not start in UEFI mode, so Tilo cannot add its startup entry.

installer-hardware-reason-startup-unknown = The startup mode could not be read.

installer-hardware-reason-input-devices = { $count } input devices were found.

installer-hardware-reason-no-keyboard = Pointing devices were found without a built-in keyboard.

installer-hardware-evidence-driver = driver { $driver }

installer-hardware-evidence-firmware-version = firmware { $firmware }

installer-hardware-evidence-vendor = vendor { $vendor }

installer-hardware-evidence-product = product { $product }

installer-hardware-evidence-problem = reports a problem

installer-hardware-evidence-display = { $name }: { $state }

installer-hardware-evidence-display-active = active

installer-hardware-evidence-display-connected = connected, not active

installer-hardware-evidence-display-disconnected = not connected

installer-hardware-evidence-kernel = running kernel { $version }

installer-hardware-evidence-acpi = { $tables } ACPI tables

installer-hardware-evidence-suspend = suspend states { $states }

installer-hardware-evidence-release = { $fact }; checked against the selected release

installer-hardware-remedy-run-on-destination = Run the check on that computer itself before installing.

installer-hardware-remedy-connect = Connect a supported adapter if you need it.

installer-hardware-remedy-connect-display = Connect the display, then check again.

installer-hardware-remedy-connect-audio = Connect a USB or Bluetooth headset if you need sound.

installer-hardware-remedy-check-driver = If it does not work after installation, check its driver then.

installer-hardware-remedy-password = Sign in with your password; fingerprint sign-in can be checked in Settings after installation.

installer-hardware-remedy-external-camera = Use an external USB camera if the built-in one shows no video.

installer-hardware-remedy-external-keyboard = Use an external keyboard if the built-in one does not work after installation.

installer-hardware-remedy-power-settings = Save your work before closing the lid, and check power settings after installation.

installer-hardware-remedy-display-settings = Turn the display on in Settings after installation.

installer-hardware-remedy-uefi = Start the installer in UEFI mode from the firmware boot menu, then check again.

installer-hardware-remedy-power-adapter = Check the power adapter, then check again.

installer-hardware-source-this-computer = Read from this computer

installer-hardware-source-this-computer-not-other = Read from this computer, not the other computer

installer-hardware-source-imported = Imported from another computer

installer-hardware-source-declared = Model named for another computer; hardware not checked

installer-preview-desktop-title = As a desktop

installer-preview-desktop-body = Use the Tilo desktop with your existing Linux system.

installer-preview-migrate-title = Migrate from this computer

installer-preview-migrate-body = Make your documents, supported apps and accounts accessible in Tilo. Your originals stay unchanged.

installer-preview-identity-heading = Have you used Tilo before?

installer-preview-new-title = This is my first time

installer-preview-new-body = Create your Tilo identity. Welcome!

installer-preview-profile-heading = Your profile

installer-preview-profile-intro = People you collaborate with will see this name and picture.

installer-preview-name-label = Your name

installer-preview-password-heading = Protect your setup

installer-preview-password-intro = Protects your identity and saved sign-ins during installation. This is separate from your computer’s login password.

installer-preview-password-label = Setup password

installer-preview-confirmation-label = Confirm setup password

installer-preview-password-hint = Use at least 15 characters, ideally several unrelated words.

installer-preview-purpose-heading = How will this device be used?
installer-preview-purpose-intro = The same Tilo OS, with defaults for the job it will do.
installer-preview-purpose-computer-title = Computer
installer-preview-purpose-computer-body = For everyday use. Choose when it sleeps in Power settings; your approved devices sync while it is awake.
installer-preview-purpose-server-title = Always-available server
installer-preview-purpose-server-body = Keeps running when idle so your approved devices can sync. Screen blanking, locking, lid and low-battery protection stay on. Use reliable power.

installer-preview-devices-heading = Name this computer

installer-preview-devices-intro = This name identifies this computer on your other Tilo devices.

installer-preview-device-name-label = Computer name

installer-preview-device-default = My computer

installer-preview-devices-another-heading = Name that computer

installer-preview-devices-another-intro = This name identifies that computer on your Tilo devices.

installer-preview-files-title = Files and settings

installer-preview-files-body = Your entire home folder, including hidden files, browser profiles, saved credentials, Flatpak apps and settings.

installer-preview-hide-details = Hide details

installer-preview-storage-heading = Choose space for Tilo

installer-preview-storage-intro = Keep your current system and install Tilo alongside it.

installer-preview-desktop-heading = Add Tilo as a desktop

installer-preview-desktop-intro = Keep your existing Linux system.

installer-preview-storage-usb-fallback-heading = Install Tilo from USB

installer-preview-storage-usb-fallback-intro = Direct installation is not available in this build. Use a USB drive to install Tilo on this computer.

installer-preview-plan-os-heading = Which system does it get?

installer-preview-os-title = Tilo OS

installer-preview-os-body = Tilo with its desktop and apps. Its login password is set on that computer at first start.

installer-preview-server-title = Tilo Server

installer-preview-server-body = No desktop and no login password. It holds your workspaces and is managed from your other devices.

installer-preview-disk-heading = Which drive does it install to?

installer-preview-disk-intro = The plan names a rule. The computer's own drives decide which one it matches, and you answer it on your devices when it matches none or several.

installer-preview-disk-largest-title = The largest drive that is not the boot drive

installer-preview-disk-largest-body = Checked against the computer's own drives when it installs.

installer-preview-disk-empty-title = A drive without data

installer-preview-disk-empty-body = Checked against the computer's own drives when it installs.

installer-preview-disk-only-title = The only drive that is not the boot drive

installer-preview-disk-only-body = Checked against the computer's own drives when it installs.

installer-preview-disk-open-title = Ask when it installs

installer-preview-disk-open-body = You choose from the drives the computer reports, before anything is written.

installer-preview-method-heading = How does the plan reach it?

installer-preview-method-intro = The medium and the commands carry the plan and one single-use code. The code expires after 24 hours; the plan can change afterwards.

installer-preview-machine-heading = Name and settings

installer-preview-machine-intro = Leave a choice open and answer it on that computer or on your devices.

installer-preview-delivery-heading = Where it goes

installer-preview-usb-title = A USB drive

installer-preview-usb-body = Write the installer to a drive you can erase, then start that computer from it.

installer-preview-netboot-title = A rescue or netboot system

installer-preview-netboot-body = Give it one command once it is running; it installs over the network.

installer-preview-alongside-title = Alongside its Linux

installer-preview-alongside-body = Give the Linux already on it one command. That system stays and starts first.

installer-preview-name-open-title = Ask when it installs

installer-preview-name-open-body = You name it when it installs, on it or on your devices.

installer-preview-network-heading = Network

installer-preview-network-dhcp-title = Automatic (DHCP)

installer-preview-network-dhcp-body = It takes its addresses from the network it joins.

installer-preview-network-open-title = Ask when it installs

installer-preview-network-open-body = Set on the computer when automatic configuration does not work.

installer-preview-unlock-heading = Unlocking it after a restart

installer-preview-unlock-tpm-title = Its own hardware protection

installer-preview-unlock-tpm-body = The drive unlocks from the computer's own protection; nobody types a password at the machine.

installer-preview-unlock-owner-device-title = One of your devices

installer-preview-unlock-owner-device-body = It asks an approved device of yours to unlock.

installer-preview-unlock-passphrase-title = A passphrase

installer-preview-unlock-passphrase-body = Someone at the machine types the passphrase.

installer-preview-unlock-open-title = Ask when it installs

installer-preview-unlock-open-body = You choose from the methods its hardware supports.

installer-preview-updates-heading = Updates it follows

installer-preview-updates-stable-title = Stable releases

installer-preview-updates-stable-body = It updates when a release is tested and announced.

installer-preview-updates-dev-title = Development releases

installer-preview-updates-dev-body = It updates with the newest changes, which may break.

installer-preview-updates-open-title = Ask when it installs

installer-preview-updates-open-body = You choose before it joins.

installer-preview-hosting-heading = Workspaces it holds

installer-preview-hosting-none-title = None for now

installer-preview-hosting-none-body = You add workspaces from your Servers entry once it has installed.

installer-preview-hosting-open-title = Ask after it installs

installer-preview-hosting-open-body = You choose which workspaces it holds, and whether it keeps encrypted copies only or takes their work, from your Servers entry.

installer-plan-heading = The plan

installer-plan-token = Carry this single-use code to the computer: { $token }

installer-plan-expiry = The code works for { $hours } more hours. If the medium is lost, only this code is exposed, and it expires.

installer-plan-carry-usb = Write the installer to a drive you can erase, and keep the code with it:

installer-plan-carry-netboot = In a rescue or netboot system on that computer, enter:

installer-plan-carry-alongside = In the Linux already on that computer, enter:

installer-plan-open-heading = What the computer still asks

installer-plan-answer-name = Its name: asked when it installs.

installer-plan-answer-network = Its network: automatic, or set on the computer when that fails.

installer-plan-answer-disk = The drive it installs to: chosen from the drives it reports, before anything is written.

installer-plan-answer-disk-drive = The drive it installs to: { $model }, { $size }.

installer-plan-answer-disk-open = The drive it installs to stays open: { $reason }

installer-plan-answer-unlock = Unlocking: chosen from the methods its hardware supports.

installer-plan-answer-updates = Updates: chosen before it joins.

installer-plan-answer-hosting = The workspaces it holds: chosen from your Servers entry.

installer-plan-answer-other = Answered when it installs.

installer-plan-unavailable = This computer cannot plan a machine. Sign in on one of your devices with tilo login, then try again.

installer-plan-waiting-heading = Computers waiting to install

installer-plan-waiting-intro = These plans have a single-use code that has not been used yet. Each opens what it still asks when its computer starts.

installer-plan-waiting-summary = { $os }, code expires in { $hours } hours.

installer-plan-waiting-unnamed = A computer without a name

installer-plan-waiting-everything-answered = Every choice is made; it installs without asking.

installer-plan-make = Make the plan

installer-plan-make-and-medium = Make the plan and choose a USB drive

installer-preview-review-heading = Review your setup

installer-preview-review-intro = Select an item to change it.

installer-preview-ready-heading = Your setup choices are ready

installer-preview-ready-intro = Nothing has been installed.

installer-preview-continue = Continue

installer-preview-back = Back

installer-preview-finish = Finish preview

installer-preview-restart = Try another path

installer-preview-choose-first = Choose an option to continue.

installer-preview-device-required = Give this device a name.

installer-preview-password-short = Use at least 15 characters.

installer-preview-password-spaces = Remove spaces at the beginning or end.

installer-preview-password-common = Use several unrelated words.

installer-preview-password-numeric = Use at least 20 digits, or add letters.

installer-preview-password-repetitive = Avoid repeated characters or patterns.

installer-preview-password-invalid = Choose a different password.

installer-preview-password-mismatch = The passwords don't match.

installer-preview-deferred-handoff = Your identity, sign-ins and files finish on the destination after its first-boot login password is set.

installer-preview-deferred-handoff-server = Your identity and sign-ins finish on the server after it installs. A server has no login password.

installer-preview-identity-busy = Wait for preparation to finish.

installer-preview-invalid-choice = That choice isn't available on this page.

installer-preview-wrong-page = This action isn't available on the current page.
installer-demo-unavailable = This action isn't available in the demo.
installer-demo-notice-title = Preview only
installer-demo-notice-body = Example data. Nothing will be installed or connected.

installer-preview-at-start = You're at the beginning of setup.

installer-preview-finished = The preview is complete. You can try another path.

installer-preview-storage-checking = Checking this computer’s storage…

installer-preview-storage-capacity = { $capacity } disk

installer-preview-storage-size-label = Space for Tilo
installer-review-allocation = { $size } GiB for Tilo.
installer-review-unallocated = Uses unallocated space; your current system’s partition stays unchanged.
installer-review-resize = Your current system’s partition will shrink from { $before } to { $after }. Its files stay in place.
installer-review-not-ready = Finish your setup choices before reviewing installation.
installer-review-storage-pending = Storage is not ready. Edit Space for Tilo and check capacity again.
installer-review-storage-shortfall = Tilo needs { $needed }; { $available } is available. Edit Space for Tilo to review capacity.

installer-preview-storage-size-range = Tilo needs at least { $min } GiB. Up to { $max } GiB is available.

installer-preview-storage-existing = Your current system

installer-preview-storage-tilo = Tilo

installer-preview-storage-temporary = Temporary installer space

installer-preview-storage-shrink = Your current system’s partition will be resized. Its files stay in place.

installer-preview-storage-shortfall = This installation needs { $needed }; { $available } is available. Choose a smaller Tilo size, or install from USB onto another disk. If you move files to other storage first, check the copy before freeing space. Nothing has been changed.

installer-preview-storage-unreadable = This disk cannot be prepared from your current system. You can use a USB installer instead.

installer-preview-storage-release-blocked = Direct installation is not available in this build. You can install from USB instead.

installer-preview-storage-unavailable = Direct installation is not available. Use a USB installer instead.

installer-preview-storage-range-error = Choose a size within the available range.

installer-preview-prepare-usb = Make a USB installer

installer-preview-save = Save changes

installer-preview-cancel = Cancel

installer-accounts-connect = Connect

installer-preview-prepare-direct = Review installation

installer-preview-install-profile-invalid = Your installation preferences need to be corrected before continuing.

installer-preview-direct-review-unavailable = Installation could not be prepared for this computer. Your setup has been kept; you can try again or make a USB installer.

installer-preview-device-invalid = Choose a shorter device name using a single line.

installer-preview-storage-show-details = Disk details

installer-desktop-prepare = Set up Tilo Desktop

installer-desktop-password-intro = Choose a password to protect your prepared Tilo identity and connections. Your computer’s login password stays the same.

installer-desktop-keep-existing = Your files, applications and Linux account stay unchanged. Your current desktop remains the default session.
installer-desktop-unavailable = Tilo Desktop setup is unavailable on this computer.

installer-desktop-retry = Try again

installer-media-capacity-checking = Checking the image size before choosing a drive.

installer-media-capacity-not-ready = The installation image size is not available yet. Go back and select a ready image, then try again.

installer-media-capacity-invalid = The space needed for this installation could not be calculated. Go back and review the image and migration choices.

installer-media-capacity-estimate = Allow at least { $required } for the installer and everything you selected. This estimate includes space for filesystem metadata.

installer-media-capacity-required = Estimated space needed: { $required }. This drive has { $available }. Choose a drive with enough space.

installer-media-drive-too-small = { $contents }. Needs { $required }: choose a drive with more room.

installer-media-ejected = { $drive } was ejected. You can take it out now.

installer-media-eject-failed = The drive was written, but it could not be ejected: { $reason } Close everything using it and try again, or shut down before you remove it.

imager-write-stopping = Stopping safely… Keep the drive connected until cancellation finishes.

imager-write-exit-unowned = The image writer's exit cannot be verified.

imager-write-worker-stopped = Writing to the drive stopped unexpectedly. Reconnect the drive, then review and retry.

imager-write-worker-start-failed = Could not start the image writer: { $error }
imager-write-cancelled = Stopped before finishing. The drive may be incomplete; review and retry before using it.

installer-review-setup = Your setup

installer-review-device-body = This device in Tilo

installer-review-usb-fallback = Direct installation isn’t enabled in this build. Make a USB installer, then restart this computer from it.

installer-review-usb-choice-required = Choose Make a USB installer to continue on this computer.

installer-review-installation-unavailable = Installation is unavailable here. Your setup choices are kept.

installer-storage-check-capacity = Check available space…

installer-storage-btrfs-resize = Your current system will keep { $after } of its { $before }. We keep at least { $reserve } above the filesystem’s required space. Your files stay in place.

installer-guided-host-unavailable = Prepare Tilo from a Linux computer. Installation setup isn’t available on this computer yet.

installer-preview-current-title = Use this computer’s identity

installer-preview-current-body = Continue with the Tilo identity already used on this computer.

# Folder review and copy-time eligibility share these explanations.
imager-folder-cloud-unavailable = Windows marks this item as online only, so it is not counted or copied and not downloaded. Make it available in Windows, then review again; this may need more disk space.
imager-folder-reparse-unsupported = This item is a Windows reparse point, such as a link, junction or cloud-managed item. This copier does not support it, even if its contents are local. Its contents are not counted or copied.

installer-hardware-reason-starts-here = This computer starts Tilo built for { $architecture }.
installer-hardware-reason-starts-board = It was built for the { $board } board.
installer-hardware-reason-starts-devicetree = It starts from its own device tree, { $devicetree }.
installer-hardware-reason-other-architecture = Tilo here is built for { $architecture }, but this computer is { $running }.
installer-hardware-reason-board-without-devicetree = The { $board } board starts { $architecture } from a device tree, and this image carries none for it.
installer-hardware-evidence-architecture = architecture { $architecture }
installer-hardware-evidence-virtualization = virtualization { $names }
installer-hardware-evidence-board = board { $board }
installer-hardware-evidence-devicetree = device tree { $devicetree }

## The destination side: the installer on the live medium.
installer-welcome-title = Welcome to Tilo
installer-welcome-note = We'll review your choices before installing. Nothing on your drives changes until you confirm.
installer-keyboard-note = Choose the layout your keyboard uses. You can change it later.
installer-account-name = Your name (optional)
installer-account-name-note = Use the name you want people to see. Your sign-in username can be different.
installer-account-username = Username
installer-account-username-note = Used to sign in and for your home directory.
installer-account-password = Your password
installer-account-password-note = This protects your files. Use 15+ characters, like a few unrelated words.
installer-account-password-confirm = Type your password again
installer-account-pin = Your startup PIN
installer-account-pin-note = Use at least 6 digits. Your computer limits wrong guesses. Keep the recovery key for hardware changes.
installer-account-pin-confirm = Type your startup PIN again
installer-storage-no-drives = No drives are available yet. Check that the drive is connected, then choose Refresh disks. Nothing has been changed.
installer-storage-erase-warning = This replaces everything on the selected drive, including its other operating systems and files. Back up anything you want to keep. Nothing is erased until you review and confirm the installation.
installer-storage-keep-guidance = Keep your other operating system and its files. Tilo needs at least { $minimum } of unallocated space or suitable empty partitions. It never shrinks or moves existing partitions. If there isn't room, you can go back safely and make space from your other operating system.

installer-media-title = Play more video and audio formats
installer-media-note = Downloaded to this computer from the formats' publishers once it is online. You can turn it off in Settings.
installer-media-install = Install
installer-media-skip = Not now
installer-media-chosen = Chosen: installed once the new system is online
installer-media-undo = Undo

installer-storage-windows-preparation = Windows or NTFS storage was detected on this disk. Before continuing, boot Windows, save its BitLocker recovery key, and turn off Fast Startup. If you still need unallocated space, shrink Windows with Disk Management. Tilo cannot make these Windows changes and will not resize NTFS.
installer-storage-linux-ready = Keep Linux: the unallocated space holds Tilo, and existing partitions stay unchanged.
installer-storage-use-capacity = Use N GiB for Tilo
installer-storage-allocation-summary = Use { $capacity } for Tilo; { $remaining } remains unused. { $details }
installer-storage-alongside-capacity = { $root } of encrypted storage for Tilo, using { $total } of unallocated space including its boot partitions.
installer-storage-linux-no-space = No safe alongside space was found. { $preparation } Nothing has been erased; you can go back without installing.
installer-storage-linux-preparation = Back up your files. Free space inside ext4 or Btrfs is not unallocated space: use your distribution's supported offline partitioning tools from recovery media to leave { $minimum } contiguous unallocated, then refresh disks. This installer will not automatically shrink existing filesystems, LVM, or LUKS containers. This medium is not a full live desktop.
installer-storage-boot-guidance = If your other system is missing from Tilo's boot menu, use the computer's firmware boot picker. Existing boot entries and the default are preserved.
installer-storage-files-guidance = Existing Linux partitions are not automatically mounted in Tilo. Files selected for import are separate from this. Set up Nextcloud separately after installation; do not share a writable sync folder between systems.

# The compatibility report on Review: the probe's own words, one message per
# component class, reason, evidence and next step. Tilo Imager shows the same
# messages for the same report.
installer-hardware-unavailable = This computer does not say enough about its model for the compatibility report, so nothing on it was checked.
installer-hardware-blocks-install = A part the desktop cannot start without is unsupported on this computer, so the installation stops at this step. Nothing has been changed.
installer-keyboard-layout = Select a keyboard layout
installer-language-locale = Choose a language and locale
installer-time-zone = Select a time zone

## Window and accessibility copy
installer-state-stopped = Stopped
installer-state-verified = Verified
installer-export-close = Press Escape to close
installer-export-profile-note = Exact embedded profile; secrets are collected on the device.
installer-recovery-key-copy-note = Copy it into a password manager or an encrypted note on another drive. The installer clears its clipboard copy after 5 minutes, or when you reboot or power off.
installer-recovery-key-copied = Copied. Paste it into a password manager or an encrypted note on another drive before rebooting.
installer-recovery-key-heading = Recovery key
installer-recovery-key-write-note = Write it down or photograph it, and keep it somewhere other than this computer. This installer has no clipboard to copy it to.
installer-installing-note = Working. This can take a few minutes; don't power off.
installer-installing = Installing Tilo…
installer-details = Details
installer-failed-note = The installer stopped before it could confirm a bootable system. The destination may already contain a partial encrypted installation. Its on-disk names, labels or metadata do not make erasing it safe. Save the recovery key below if one was generated, then review storage or use Ctrl+Alt+F2 for recovery tools.
installer-failed = Installation failed
installer-boot-shortcuts = Tilo can add menu shortcuts for unambiguous loaders on the shared EFI partition. If Linux is not listed, use the computer's firmware boot picker. Loaders on other EFI partitions may not be listed.
installer-boot-order = To choose Tilo or your existing system, open this computer's firmware boot picker. The installer preserved its previous default.
installer-installed = Tilo is installed
installer-recovery-key-note = A recovery key is shown during install. It is your only backup if the TPM is reset or fails, so save it somewhere safe.
installer-image-note = This install uses the Tilo image on this medium; it does not download an update. Future system updates come from the signature-required Tilo repository and are checked automatically after first boot whenever the computer is online.
installer-review = Review your choices
installer-filesystem-recommended = Recommended for Tilo
installer-filesystem = Filesystem
installer-time-zone-pick = Pick from the list, or start typing to filter (IANA name, e.g. Europe/Amsterdam or UTC).
installer-language-note = This controls language, formatting, and regional conventions after installation.
installer-a11y-language-locale = Language & locale
installer-hostname-note = The name your computer uses on the network.
installer-hostname = Hostname
installer-security-key-option = Optionally enroll a FIDO2 security key (e.g. a YubiKey) as another way to unlock. Plug it in before installing.
installer-security-key = Security key
installer-encryption-always = Always on, with LUKS2
installer-disk-encryption = Disk encryption
installer-options-heading = Installation options
installer-recovery-tools = Need files from this computer, or removing an alongside Tilo install? Press Ctrl+Alt+F2 for recovery tools; Ctrl+Alt+F1 returns here.
installer-account-create = Create your account
installer-account-secure = Secure your account
installer-account-server-title = Protect this server's disk
installer-account-server-note = There is no sign-in on this machine. It joins your identity on first start, and you manage it from your other devices.
installer-account-server-secret = Disk unlock secret
installer-account-server-secret-note = Needed whenever the server restarts. Keep the recovery key somewhere safe.
installer-account-server-secret-confirm = Type the disk unlock secret again
installer-region-measured = The location is measured from the start of the selected disk. Existing partitions between free regions remain untouched.
installer-region-choose = Choose the exact unallocated region for Tilo
installer-disk-protected = This disk is hardware or policy write-protected.
installer-install-tilo = Install Tilo
installer-wifi-saved = Wi-Fi connections made here are saved for first boot. You can also continue without one.
installer-network-privileged = For safety, the privileged live installer does not open portal web content. Continue offline, then open this address after first boot.
installer-wifi-password = Wi-Fi password
installer-network-optional = Optional. Tilo installs from this medium and needs no internet access.
installer-network-connect = Connect to a network

## Facts, property labels and section titles
imager-selected-image = Selected image: { $name }
imager-section-recent = Recent
imager-section-releases = Tilo releases
imager-a11y-erase-drive = Erase and write { $drive }
installer-property-network = Network
installer-property-name = Name
installer-property-username = Username
installer-property-formats = More video and audio formats
installer-property-keyboard = Keyboard layout
installer-property-timezone = Time zone
installer-property-hostname = Hostname
installer-property-unlocks-with = Unlocks with
installer-property-encryption = Encryption
installer-property-filesystem = Filesystem
installer-property-existing-files = Existing files
installer-property-boot-choice = Boot choice
installer-property-choose-os = Choose an OS
installer-property-tilo-efi = Tilo EFI partition
installer-property-tilo-root = Tilo root
installer-choose-os-restart = The next restart starts Tilo once; { $host } stays the firmware default
installer-property-installer-space = Installer space
installer-disk-holds-installer = Also holds the running installer and your existing { $host } system. { $host } stays untouched.
installer-property-this-disk = This disk
installer-property-existing-partitions = Existing partitions
installer-property-storage = Tilo storage
installer-property-method = Install method
installer-property-disk = Disk
installer-type-to-confirm = Type { $expected } to confirm the { $operation }
installer-install-alongside = Installed alongside { $host } in the space Tilo Imager prepared. This disk also holds the running installer; { $host } and its files are not changed.
installer-wifi-unavailable = Wi-Fi controls unavailable: { $error }
installer-network-signin-unavailable = A sign-in page was detected but cannot be opened: { $error }
installer-network-signin-address = Network sign-in address: { $uri }
imager-section-before-installing = Before installing
imager-section-full-download = Full Tilo download
imager-section-desktop = Tilo Desktop

imager-section-selected-image = Selected image
installer-install-unavailable = { $message }. Installation is unavailable.
installer-operation-erase = whole-disk erase
installer-operation-guided-plan = guided free-space partition plan
installer-operation-tilo-format = Tilo boot and root format

## What the window shows while it writes and installs
imager-filesystem-help = Btrfs is recommended for Tilo.
imager-drive-empty = Empty disk
imager-drive-partitions = { $count ->
    [one] 1 partition
   *[other] { $count } partitions
}
imager-drive-partitions-held = { $count ->
    [one] 1 partition
   *[other] { $count } partitions
} ({ $filesystems })

imager-write = Write
imager-write-driving = Erase and write the selected drive
imager-write-cancel = Cancel write
imager-write-cancelling = Stopping…
imager-eject = Eject
imager-start-over = Start over
imager-review-and-retry = Review and retry
imager-browse = Browse…
imager-release-cancel = Cancel download
imager-preview-embedded = Preview embedded setup
imager-release-progress = { $stage }, { $percent }%
imager-release-channel = { $target }, { $channel } channel
imager-release-channel-downloads-first = { $target }, { $channel } channel, downloads first
imager-writing-verify-now = Reading the drive back to check every byte.
imager-writing-verify-next = Then the drive is read back and checked.
imager-writing-from-to = { $source } to { $target }. Keep the drive connected until this finishes.
imager-storage-device = { $size } storage device
imager-installer-title = Tilo installer
imager-installer-guidance = Installation guidance
imager-installer-continue = Continue
imager-installer-continue-offline = Continue offline
imager-installer-next = Next
imager-installer-preflight-failed = Could not launch the prepared-storage check: { $detail }
imager-installer-connecting = Connecting to { $network }…
imager-installer-checking-networks = Checking networks…

# What the wizard's own buttons say, one message each; the pages the imager's
# windows stand in; and the words the image wizard and the download status use.
installer-step-keyboard = Keyboard
installer-step-network = Network
installer-step-disk = Disk
installer-step-account = Account
installer-step-options = Options
installer-step-media = Media
installer-step-review = Review
installer-step-install = Install
installer-action-power-off = Power off
installer-action-reboot = Reboot now
installer-action-return-to-host = Return to { $host }
installer-action-refresh-networks = Refresh networks
installer-action-refresh-disks = Refresh disks
installer-action-review-storage = Review storage
installer-action-recovery-promise = I will save my recovery key
installer-action-copy-recovery-key = Copy recovery key
installer-action-copy-recovery-key-again = Copy recovery key again
installer-action-recovery-saved = I saved this key off this computer
installer-action-begin = Begin Install
installer-region-use = Use { $size } free at { $start }
installer-region-selected = Selected: { $size } free at { $start }
imager-page-choose-image = Choose an image
imager-page-choose-drive = Choose a drive
imager-page-prepare-setup = Prepare the setup
imager-page-review-install = Review and install
imager-page-review-write = Review and write
imager-drive-unnamed = this drive
imager-confirm-system-disk = This is your system disk
imager-confirm-internal-disk = Everything on the internal disk { $device } is erased
imager-confirm-disk = Everything on { $device } is erased
imager-confirm-system-consequence = Everything on it is erased, the running system included. This cannot be undone.
imager-confirm-consequence = This cannot be undone.
imager-writer-unavailable = This computer can't write drives yet.
imager-image-unreadable = That file is not a readable disk image.
imager-write-done = { $name } is written and verified.
imager-install-done = { $name } is installed and verified.
imager-write-failed = Writing { $name } failed. Reconnect the drive, then review and retry.
imager-install-failed = Installing { $name } failed. Reconnect the drive, then review and retry.
imager-drop-title = Choose a disk image
imager-drop-message = Drop an ISO, IMG or RAW file here
imager-empty-drives = Insert a USB drive or SD card
imager-no-releases = No Tilo releases are stored on this computer.
imager-plain-body = Setup options are only available for Tilo images. This image is written unchanged.
imager-setup-internal-download-title = Full Tilo download
imager-download-title-downloading = Downloading Tilo
imager-download-title-ready = Download ready
imager-download-title-paused = Download paused
imager-download-title-cancelled = Download cancelled
imager-download-title-failed = Download failed
imager-download-title-released = Kept for later
imager-download-title-discarded = Unfinished download data discarded

imager-network-connected = Connected
imager-network-secured = Secured
imager-network-open = Open
imager-installer-select-disk = Select a disk to see how Tilo can share it.
imager-installer-format-prepared = Format only the prepared Tilo boot and root partitions; the existing EFI and other OS partitions stay as they are.
imager-installer-no-disk = No disk chosen
imager-installer-erase = This will erase { $device } and all data on it.
imager-installer-complete-host = Installation complete. Restart to start Tilo; { $host } stays the default in the firmware boot order. The temporary installer space is returned to Tilo after you first sign in. Signed system updates are checked after first boot when online; no update was downloaded during this install.
imager-installer-complete = Installation complete. Remove the installer medium, then reboot. Signed system updates are checked after first boot when online; no update was downloaded during this install.
imager-installer-refresh-none = Disk list refreshed; no installable disks were found.
imager-installer-refresh-review = Disk list refreshed. Review the destination before continuing.
imager-installer-refresh-select = Disk list refreshed. Select a destination again.
imager-desktop-checking = Checking this computer and the signed Tilo Desktop catalog…
imager-desktop-unavailable = Tilo Desktop isn't available for { $host } yet.
imager-desktop-already-installed = Tilo Desktop { $version } is already installed on { $host }. Continue to check the Tilo session; approve the check when asked.
imager-desktop-ready = Tilo Desktop { $version } is ready for { $host }. Continue to download it; approve the installation when asked.
imager-desktop-downloading = Downloading and checking Tilo Desktop { $version } for { $host }. Cancelling now stops before anything is installed.
imager-desktop-authorizing = Waiting for you to approve installing Tilo Desktop { $version } on { $host }.
imager-desktop-preparing = Checking Tilo Desktop { $version } for { $host } once more before installing.
imager-desktop-installing = Installing Tilo Desktop { $version } on { $host }. Closing setup does not stop the installation.
imager-desktop-verifying = Checking the installed Tilo session on { $host }.
imager-desktop-installed = Tilo Desktop { $version } is installed and verified on { $host }. Save your work and sign out when you are ready, then choose Tilo on the login screen. Your current session stays the default.
imager-desktop-download-cancelled = The download was cancelled.
imager-desktop-not-approved = The installation was not approved.
imager-desktop-needs-admin = Installing needs an administrator's approval.
imager-desktop-stopped-early = Checking this computer stopped early.
imager-desktop-not-ready = This computer is not ready for Tilo Desktop.
imager-desktop-download-failed = The download failed.
imager-desktop-install-failed = The installation failed.
imager-desktop-failed = { $what } { $detail }
imager-desktop-failed-retry = { $what } { $detail } Your folder choices are kept; check again to retry.
imager-retained-windows = Windows stays installed and remains the startup default.
imager-retained-linux = Your Linux installation stays as it is and remains the default session.
imager-retained-other = Your current system stays installed and remains the default.
imager-desktop-unsupported-component = A component needed for the desktop is unsupported. Resolve it below, then check again.

## What the destination step and the copy say
installer-battery-power = The battery is at { $percent }%. Connect this computer to power before installing.
installer-fs-btrfs = Snapshots, compression and rollback.
installer-fs-ext4 = Simple and battle-tested, with no snapshot support.
installer-fs-xfs = High throughput for large files, with no snapshot support.
installer-encrypt-verified = Tilo encrypts the disk with LUKS2 and binds unlock to this computer's Secure Boot state plus the release-authorized Tilo boot measurement. You will use a short startup PIN; a recovery key is shown once during install.
installer-encrypt-target-unverified = Tilo always encrypts the disk with LUKS2. Verified boot is unavailable for this installation, so use one strong startup passphrase. A short PIN is only safe when the TPM can verify the complete boot chain.
installer-encrypt-lockdown-off = Tilo always encrypts the disk with LUKS2. Secure Boot is active but kernel lockdown is not, so Tilo cannot treat this boot as verified and uses a strong disk passphrase instead.
installer-encrypt-secure-boot-off = Tilo always encrypts the disk with LUKS2. TPM2 short-PIN protection requires verified boot, but Secure Boot is not active, so this installation uses a strong disk passphrase instead.
installer-encrypt-no-tpm = Tilo always encrypts the disk with LUKS2. This computer has no TPM2 chip, so you will enter a disk passphrase at startup. A recovery key is shown once during install as a backup.
installer-filesystem-cannot-boot = { $filesystem } cannot boot this image; choose a supported filesystem.
installer-image-official = Official Tilo release, verified
installer-image-local = Local image, written unchanged

## The wizard's reasons, items and stations, and the writer's progress
installer-security-key-off = No security key
installer-security-key-enroll = Enroll security key
installer-method-erase = Erase disk
installer-method-alongside = Install alongside
installer-alongside-change-guided = Three new Tilo partitions (EFI, boot, and encrypted root) will be created and formatted only in the reviewed unallocated space on { $device }. Existing OS partitions are preserved.
installer-alongside-change-prepared = Only the prepared Tilo boot and root partitions on { $device } will be formatted. The existing EFI partition and other OS partitions are preserved.
installer-alongside-change-note = { $change } Firmware/Secure Boot changes can still trigger a Windows BitLocker recovery prompt; save that recovery key before continuing.
installer-state-idle = Waiting
installer-state-running = Installing
installer-state-done = Installed
installer-state-failed = Failed
installer-blocked-other-step = This belongs to the { $wanted } step; the installer is on { $current }. Use next or back to get there.
installer-blocked-password-hash = A password hash is accepted only by an explicit unattended installation.
installer-blocked-next-review = The review ends with starting the installation, not with Next.
installer-blocked-next-install = The install has started; there is no next step.
installer-blocked-back-install = The install has started; the wizard cannot go back.
installer-blocked-back-first = This is the first step.
installer-blocked-not-staged = This installer was not staged next to an existing system.
installer-blocked-return-running = The install is running; wait for it to finish or fail.
installer-blocked-return-done = Tilo is installed. Restart this computer to start it.
installer-blocked-network-busy = A network operation is already in progress.
installer-blocked-no-networks = No Wi-Fi networks are visible; refresh the list.
installer-blocked-no-network-choice = Choose a Wi-Fi network first.
installer-blocked-network-connected = That Wi-Fi network is already connected.
installer-blocked-no-recovery-key = No recovery key has been generated.
installer-blocked-no-clipboard = This installer has no clipboard to copy the recovery key to. Write it down instead.
installer-blocked-power-not-finished = Power actions are offered once the install has finished.
installer-blocked-recovery-unsaved-reboot = Save the recovery key off this computer and confirm it before restarting.
installer-blocked-recovery-unsaved-power-off = Save the recovery key off this computer and confirm it before switching off.
installer-blocked-installer-disk = The running installer disk cannot be used as its own destination.
installer-blocked-read-only = This disk is hardware or policy write-protected.
installer-blocked-disk-too-small = This disk is too small: Tilo needs at least { $minimum }.
installer-blocked-begin-no-target = Select a disk (and, alongside, a region) before installing.
installer-blocked-begin-recovery = Confirm you will save your recovery key first.
installer-item-region = { $size } free at { $start }
installer-item-storage = Installation target
installer-item-storage-erase = Erase the selected disk
installer-item-review = Review and install
installer-item-review-no-target = Select a destination first.
installer-review-erase = This erases { $device } and all data on it, then installs Tilo. Destructive: type the device name as the confirmation phrase.
installer-review-alongside = { $impact } { $boot } { $files } Destructive: type the confirmation phrase.
installer-detail-step = Step { $number } of { $count }: { $step }
installer-status-type-confirmation = Type { $phrase } exactly to confirm formatting it.
imager-phase-verifying = Verifying
imager-phase-installing = Installing
imager-phase-writing = Writing
imager-phase-idle = Ready
imager-phase-success = Complete
imager-phase-failed = Failed
imager-stage-preparing = Preparing
imager-stage-downloading = Downloading
imager-stage-verifying = Verifying
imager-status-drive-disconnected = The drive was disconnected while it was being written. Reconnect it, then review and retry.
imager-status-selected = Selected { $name } — { $kind }
imager-status-official = official release verified
imager-status-local = local image, written as-is
imager-picker-images = Disk images
imager-picker-choose = Choose
imager-status-pick-cancelled = File selection cancelled.
imager-status-download-busy = A Tilo installer download is already in progress.
imager-status-downloading = Downloading and verifying { $name }…
imager-status-download-cancelling = Cancelling safely… The verified partial download will be kept for resume.
imager-status-download-progress = Downloading Tilo installer… { $percent }%
imager-status-download-verifying = Cryptographically verifying the Tilo installer…
imager-status-download-preparing = Preparing the verified Tilo installer image…
imager-status-download-verified = Tilo installer downloaded and cryptographically verified.
imager-status-internal-disk = Internal and system disks can only be installed from the booted Tilo medium.
imager-status-writing-to = { $name } → { $drive } (writing)
imager-status-review-and-confirm = Review the image and drive, then confirm the target again to retry.
imager-status-customized = Image customization complete
imager-status-opening-picker = Opening the file picker…
imager-status-no-download = No Tilo installer download is running
imager-status-no-write = No image is being written
imager-status-nothing-to-eject = No written drive is waiting to be ejected
imager-status-write-phase = Image write: { $phase }
imager-row-download = Download { $name }
imager-write-refused-busy = An image write is already in progress
imager-write-refused-disabled = Raw-device writing is disabled; enable the privileged Imager backend first
imager-write-refused-incomplete = Select an image source and target drive before writing
imager-write-refused-no-window = The writer could not start in this window.
imager-write-refused-confirmation = Confirmation does not match — write refused.
imager-write-refused-too-large = Image is larger than the drive — write refused.
imager-write-refused-selection = Select a source and drive before writing.
imager-write-refused-restoring = Restoring the previous image-write state; wait for drive review before starting another write
imager-writing-eta = about { $time } left
imager-step-source = Source
imager-step-drive = Drive
imager-step-customize = Customize
imager-step-confirm = Review
imager-step-write = Write
imager-step-done = Done

## What the setup says while it works
imager-setup-status-choose-experience = Choose how to set up Tilo to begin.
imager-setup-status-rechecking-before-download = Checking this computer again before the download…
imager-setup-status-review-last = The review is the last setup step.
imager-setup-status-cancelling-package = Cancelling the download…
imager-setup-status-rechecking-before-retry = Checking this computer again before the retry…
imager-setup-error-download-not-offered = No Tilo download is offered right now.
imager-setup-error-download-multiple = More than one Tilo download is offered for this computer.
imager-setup-error-download-catalog = The download list could not be read.
imager-setup-status-downloading = Downloading Tilo…
imager-setup-status-pausing-download = Pausing the download…
imager-setup-error-no-saved-download = No saved download is available.
imager-setup-status-cancelling-download = Cancelling the download…
imager-setup-error-no-unfinished-download = No unfinished download data is available.
imager-setup-status-discarding-download = Discarding unfinished download data…
imager-setup-error-no-verified-download = No verified download is held.
imager-setup-status-keeping-download = Keeping the download for later…
imager-setup-status-staging-unavailable = Nothing was installed yet. Protected staging is not available on this computer.
imager-desktop-installed-sign-out = Tilo Desktop is installed and verified. Sign out when you are ready.
imager-desktop-cannot-download = Tilo cannot be downloaded on this computer, so nothing was installed.
imager-setup-internal-download-cancelled-detail = The download was cancelled before it finished.

## What the download card says under its heading
imager-setup-internal-download-choose = Choose Download Tilo after reviewing this setup.
imager-download-sentence-checking-release = Checking the Tilo release.
imager-download-sentence-checking-earlier = Checking what was downloaded earlier.
imager-download-sentence-downloading = Downloading and checking Tilo.
imager-download-sentence-ready = The download is verified. Nothing has been installed.
imager-download-sentence-paused = Paused. Resuming checks what was downloaded first.
imager-download-sentence-cancelled = Cancelled. Downloaded data stays until you discard it.
imager-download-sentence-failed = The download did not finish.
imager-download-sentence-released = The verified download is kept on this computer. Nothing has been installed.
imager-download-sentence-discarded = Unfinished download data was removed. Completed parts may stay.
imager-download-sentence-handoff-failed = The verified download is kept; the installation stopped.

## What the wizard reports about the target it was given
installer-status-install-start = Starting the install to { $disk } for user { $user } ({ $filesystem }).
installer-status-install-start-server = Starting the install to { $disk } ({ $filesystem }).
installer-blocked-no-such-disk = No disk { $device } is listed; refresh the list.
installer-error-unknown-filesystem = Unknown filesystem: { $filesystem }
installer-keyboard-map-name = { $layout } keyboard layout
## The window bar's Rescan
imager-rescan-drives = Rescan drives

## The customization form's sections and fields
imager-profile-section-identity = Identity
imager-profile-section-region = Region
imager-profile-section-storage = Storage
imager-profile-section-network-identity = Network and identity
imager-profile-hostname = Hostname
imager-profile-full-name = Full name
imager-profile-username = Username
imager-profile-locale = Language and locale
imager-profile-timezone = Time zone
imager-profile-keyboard = Keyboard layout
imager-profile-filesystem = Filesystem
imager-profile-hostname-required = Enter a hostname.
imager-profile-username-required = Enter a username.
imager-profile-required = Enter a value.
imager-profile-choose-listed = Choose a value from the list.
imager-profile-choose-listed-field = Choose a listed { $field }.
imager-write-setup-wifi-ssid = Wi-Fi network
imager-write-setup-wifi-psk = Wi-Fi password
imager-write-setup-ssh-key = Owner SSH public key
imager-write-setup-device-label = Device name
imager-write-setup-enroll = Enroll as a new device
imager-write-setup-enroll-note = The new device becomes one of yours. Your account key stays on this computer.

## Where a release runs, and the filesystem it installs
imager-release-target-arm64 = Generic ARM64
imager-release-target-x86-64 = Generic x86-64
imager-release-target-media = Generic media
imager-release-target-profile = { $profile } only
imager-filesystem-btrfs = Btrfs
imager-filesystem-ext4 = Ext4
imager-filesystem-xfs = XFS
installer-network-via = Internet via { $ssid } (saved for first boot)
installer-network-available = Internet available
installer-network-offline-available = { $connectivity }. Offline installation remains available.
installer-connectivity-full = Connected to the internet
installer-connectivity-portal = Sign-in required
installer-connectivity-limited = Limited connectivity
installer-connectivity-offline = Offline
installer-connectivity-unknown = Connectivity not checked

## Actions

imager-destination-set-media-codecs = Install media codecs
imager-setup-action-install-desktop = Install Tilo Desktop

## What the app is called in a window and in the launcher
installer-app-title = Tilo Installer

## Action and resource names, as the palette, agents and console list them
imager-destination-back = Back
imager-destination-begin = Begin installation
imager-destination-connect-network = Connect to the chosen Wi-Fi network
imager-destination-next = Next
imager-destination-return-to-host = Return to the existing system
imager-destination-select-disk = Use this disk
imager-destination-select-network = Choose Wi-Fi network
imager-destination-select-region = Use this region
imager-destination-set-disk-passphrase = Set a separate disk startup secret
imager-destination-set-filesystem = Choose filesystem
imager-destination-set-hostname = Set hostname
imager-destination-set-identity = Set username and name
imager-destination-set-layout = Choose keyboard layout
imager-destination-set-locale = Choose language and locale
imager-destination-set-method = Choose install method
imager-destination-set-passphrase = Set account and default startup secret
imager-destination-set-password-hash = Set a pre-hashed account password
imager-destination-set-security-key = Enroll a security key
imager-destination-set-timezone = Choose time zone
imager-destination-status = Installer status
imager-installer-preview-back-in-preview = Return to the preceding page
imager-installer-preview-check-storage = Inspect this computer's disk again
imager-installer-preview-choose-and-continue-preview = Choose an option and continue
imager-installer-preview-choose-preview = Choose an option on this page
imager-installer-preview-clear-target-hardware-report = Stop using the imported hardware report
imager-installer-preview-continue-preview = Continue to the next page
imager-installer-preview-edit-preview = Change a section of the setup
imager-installer-preview-import-target-hardware-report = Import the other computer's hardware report
imager-installer-preview-open-migrations = Open Migrations
imager-installer-preview-prepare-desktop-installation = Prepare Tilo Desktop for this user
imager-installer-preview-prepare-direct-installation = Prepare the installation on this computer
imager-installer-preview-prepare-usb = Open the USB media writer
imager-installer-preview-read-preview = Read the setup's choices
imager-installer-preview-recheck-target-hardware = Check the other computer's description again
imager-installer-preview-restart-preview = Start the setup again
imager-installer-preview-set-migrate-ticket = Enter the ticket from tilo device add
imager-installer-preview-set-preview-confirmation = Confirm the unlock password
imager-installer-preview-set-preview-device-name = Set this device's name
imager-installer-preview-set-preview-password = Enter the unlock password
imager-installer-preview-set-storage-size = Choose the space for Tilo
imager-installer-preview-set-target-architecture = Choose the architecture the medium starts on
imager-installer-preview-set-target-identity-field = Name the other computer
imager-installer-preview-sign-in-for-migration = Sign in to your workspace with the ticket
imager-installer-preview-toggle-storage-details = Show or hide the disk's breakdown
imager-setup-cancel-desktop-download = Cancel the download
imager-setup-read-desktop-status = Read Tilo Desktop readiness on this computer
imager-setup-read-setup-status = Read setup status
imager-setup-recheck-desktop = Check Tilo Desktop availability again
imager-sources = Image sources
imager-sources-clear-source = Clear the chosen image
imager-sources-select-source = Choose an image to write
imager-write-plan-cancel-download = Cancel the installer download
imager-write-plan-cancel-write = Stop writing the image
imager-write-plan-choose-drive = Choose target drive
imager-write-plan-choose-release = Choose a Tilo release
imager-write-plan-customize-image = Customize an image file
imager-write-plan-eject = Eject the written drive
imager-write-plan-export-config = Export headless configuration
imager-write-plan-set-ssh-key = Set the owner's SSH key
imager-write-plan-set-wifi-psk = Set Wi-Fi password
imager-write-plan-status = Show write status
imager-write-plan-write = Erase drive and write image

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
