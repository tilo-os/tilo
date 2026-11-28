# The runtime's own words: the camera and microphone consent prompt.
# $device is `camera` or `microphone`.
device-consent-question = { $device ->
    [camera] Allow { $app } to use the camera?
   *[microphone] Allow { $app } to use the microphone?
}
device-consent-detail = { $device ->
    [camera] { $app } gets nothing from the camera unless you allow it. Your choice holds on this device until you take it back from any of your devices.
   *[microphone] { $app } gets nothing from the microphone unless you allow it. Your choice holds on this device until you take it back from any of your devices.
}
device-consent-allow = Allow
device-consent-deny = Don’t Allow

# Where an agent of this login works on another of the person's devices: $device as it was
# named, $devices the ones known here.
other-devices-none = None of your other devices is known here.
other-devices-unknown = None of your devices here is called { $device }. Your devices here: { $devices }.
other-devices-name-workspace = { $device } is in more than one of your workspaces here. Name one with --workspace.

# The layout's own actions, as the window manager of a workspace reads them.
so-tilo-layout-read = Read the layout: the workspaces, strips and windows on it, the app and record each window shows, and the other workspaces the caller reaches here.
so-tilo-layout-place = Open a window of an app on the layout, as the caller's: beside the window in front, on the record it names or the app's own. A record a window shows already stays where it is.
so-tilo-layout-close = Close a window on the layout, everywhere it is shown. Its record stays in the workspace.
so-tilo-layout-position = Stand a window at a point on its workspace's canvas, as a drag that let it go there leaves it: one arranging step for the whole gesture.
so-tilo-layout-size = Give a window a size on its workspace's canvas, as dragging its edge does.
so-tilo-layout-activate = Bring a window in front of its strip, and its workspace forward, as a press on its bar does.

# What a device's package cache offers, as its daemon publishes it.
so-tilo-packages-collect = Reclaim what nothing needs
so-tilo-packages-verify = Check the installed apps against their packages
so-tilo-packages-install-dev = Install a built app as this device's development build
so-tilo-packages-runtime-install = Build and install a runtime
so-tilo-packages-runtime-fetch = Fetch and install a runtime a repository offers
so-tilo-packages-runtime-remove = Remove a runtime
so-tilo-packages-runtime-pin = Keep a runtime
so-tilo-packages-runtime-unpin = Let a runtime go when nothing runs on it
so-tilo-packages-image-add = Add an environment image
so-tilo-packages-image-remove = Remove an environment image
so-tilo-packages-image-fetch = Fetch an environment image
so-tilo-packages-flatpak-remote-add = Add a Flatpak repository
so-tilo-packages-flatpak-remote-remove = Remove a Flatpak repository
so-tilo-packages-flatpak-remote-enable = Enable a Flatpak repository
so-tilo-packages-flatpak-remote-disable = Disable a Flatpak repository
so-tilo-packages-flatpak-flathub = Add Flathub
so-tilo-packages-flatpak = What this device's Flatpak installation has
so-tilo-packages-flatpak-catalogue = The apps a Flatpak repository offers
so-tilo-packages-catalogue = The apps a Tilo repository offers
so-tilo-packages-runtime-offer = The build of a runtime a Tilo repository offers

# The steps of an agent session's tools, as the daemon lists them.
agent-tools-resource = Agent tools
agent-tools-list = List the tools
agent-tools-list-detail = Lists the tools an agent works through in its session.
agent-tools-call = Call a tool
agent-tools-call-detail = Calls one of the agent's tools as a step of its session, and answers what it said.

# What the person's shell tells this device's apps about their windows, as the daemon lists it.
window-follow-resource = Windows
window-follow-follow = Follow someone's view
window-follow-follow-detail = Tells a workspace's apps on this device whose view you follow.
window-follow-handoff = Move a window
window-follow-handoff-detail = Asks an app on this device for the next step of moving its window to another device.

# The calls of a workspace's live session this device joins and leaves, as the daemon lists them.
live-call-resource = Live session
live-call-join = Join
live-call-join-detail = Join a call of this workspace on this device.
live-call-leave = Leave
live-call-leave-detail = Leave the call this device is in here.

# Nearby Send between the person's devices, as the daemon carries it and their shell shows it.
nearby-send-resource = Nearby Send
nearby-send-offer = Send a file to a device
nearby-send-offer-detail = Offers a file this device already holds to another of your devices, which chooses where it lands.
nearby-send-keep = Keep a file a device sent
nearby-send-keep-detail = Keeps a file another of your devices offered, in the folder you choose.
nearby-send-refuse = Turn down a file
nearby-send-refuse-detail = Turns down a file another of your devices offered.
nearby-send-stop = Stop a transfer
nearby-send-stop-detail = Stops a transfer going out or coming in.
nearby-send-again = Offer a file again
nearby-send-again-detail = Offers a transfer that failed again.
nearby-send-piece = Read a piece of an offered file
nearby-send-piece-detail = Reads one piece of a file this device offered, for the device taking it.
nearby-send-keep-question = Keep “{ $name }” from { $device }?

## The copies the person's devices carry between them

so-tilo-clipboard-share = Share what this device copied
so-tilo-clipboard-receive = Take what another device copied
so-tilo-nearby-receive = Take another device's offer, so its person can answer it here

## Workspaces the device that founds them makes for the person's other devices


## Keys the person approves for their programs, certified by their root here

so-tilo-keys-certify = Make a key for one of your programs
keys-certify-question = Make the key “{ $name }”, which acts as you in { $reach }?
keys-certify-every-workspace = all your workspaces
keys-certify-one-workspace = one of your workspaces

## The actions of this device's server settings

so-tilo-device-server-set-service = Offer or withdraw a service
so-tilo-device-server-set-work = Whose work this device takes
so-tilo-device-server-set-work-room = Keep part of the machine free
so-tilo-device-server-set-work-while-charging = Take work only while charging

## Where this device is

so-tilo-device-location-locate = Where this device is now, as its latitude and longitude

# A computer of the person's asks their identity to certify the key it unlocks its disk with at
# boot: what they read where they answer, and what the computer is told. $computer is its name.
boot-unlock-asked = Set up unlock for { $computer }?
boot-unlock-detail = It will unlock its disk at boot with your devices’ help.
boot-unlock-unattended = It will then start without asking you again.
boot-unlock-computer = Computer
boot-unlock-key = Boot key
boot-unlock-declined = You said no on your other device, so this computer won’t unlock its disk with your devices. It still opens with its passphrase or recovery key.
boot-unlock-unanswered = Nobody answered on your other device in time, so this computer won’t unlock its disk with your devices. Try again when you’re at that device.

# An early-boot device of the person's asks another of their devices for the half of its disk key
# it cannot read alone. $device names the device that asks, by the name the person gave it; $slot
# is its disk's volume and key slot. The code on its boot screen is a fact beside these words.
unlock-asked = Unlock { $device }?
unlock-detail = It is starting up and asks you for the half of its disk key that this device holds for it, for { $slot }. Compare the code with the one on its boot screen.
unlock-code = Code on its screen
unlock-gone = The device that asked stopped waiting
unlock-unanswered = Nobody answered in time, so the device was not unlocked

# A program on this computer that dialled its daemon itself asks for what only the person's root
# signs: $path is the program's file, as the system names it.
asked-by-program = a program ({ $path })
asked-by-a-program = a program
asked-program-unanswered = No device of yours answered, so nothing changed.
boot-unlock-no-root-device = None of your devices that keep your identity can be reached right now. Turn one on, connect it to the internet and sign in, then try again.

# Another of the person's devices speaks a peer protocol version this one does not work with:
# $device is the name their record gives it. The side that updates is the side that is behind.
system-update-app-name = Updates
version-mismatch-updating-title = { $device } runs a newer Tilo
version-mismatch-updating-body = This device updates and restarts onto it once your screen has been locked for a while.
version-mismatch-needs-update-title = { $device } must update
version-mismatch-needs-update-body = Update Tilo there, then both work together again.
version-mismatch-unknown-updating-title = Another device runs a newer Tilo
version-mismatch-unknown-updating-body = This device updates and restarts onto it once your screen has been locked for a while.
version-mismatch-unknown-needs-update-title = Another device must update
version-mismatch-unknown-needs-update-body = Update Tilo there, then both work together again.

so-tilo-browser-open = Open Browser

# The workspace's own folder on this device, as its actions list it.
device-files-open = Put in Files
device-files-new-folder = Make a folder
device-files-rename = Rename
device-files-move = Move
device-files-copy-into = Copy
device-files-trash = Move to Trash
device-files-restore = Put back
device-files-write-file = Write a file
device-files-undo = Take back
offload-place = Place

# This device and the person's other devices, as their actions list them.
device-info = This device
device-logIn = Log in with a ticket
device-issue = Add a device
device-rename = Rename a device
device-revoke = Revoke a device
device-identities = List the identities
device-useIdentity = Use an identity for new apps
device-removeIdentity = Remove an identity
device-protectIdentity = Protect an identity's key

# The device's own actions, its servers', its updates' and its settings', as its listing names them.
device-answerSignIn = Answer a sign-in
device-approve = Approve a browser's sign-in
device-backup = Back up the person
device-booted = An update booted
device-certifyKey = Certify a key
device-checkCallerProof = Check a caller's proof
device-dev-id = A development build's id
device-drain = Drain a server
device-endorseImage = Endorse a system image
device-floor = Ask the rollback floor to move
device-found = Start as a new person
device-journal = Read a server's journal
device-openTerminal = Open a hosted Terminal shell
device-openVault = Open a Secret Service vault
device-pause-updates = Stop a server taking updates
device-pending = Sign a browser in
device-prepare = Take this server's update turn
device-reboot = Reboot a server
device-recoveryWords = Show the recovery words
device-releaseHalf = Release a boot's unlock half
device-remove = Remove a setting
device-renewCertificates = Renew my certificates here
device-reported = Report a login's step
device-restart = Restart a service on a server
device-restore = Restore a person
device-resume-updates = Have a server take updates again
device-rollback = Have a server start the previous system
device-sealVault = Seal a Secret Service vault
device-set = Change a setting
device-set-default-start = Have Tilo start by default on a server
device-setUpUnlock = Enrol this computer's disk unlock
device-sign = Sign as this device
device-stage-update = Stage an update on a server
device-status = Ask a server how it is
device-terminalRecords = Read or write Terminal's environments
device-update = Update a device

# The workspace's environments, Wi-Fi passphrases and approved checks, as their listings name them.
environment-create = Create an environment
environment-list = List the environments
wifi-lookup = Find a shared Wi-Fi passphrase
wifi-share = Share a Wi-Fi passphrase with your devices
approved-checks-run = Run an approved check

# The seats' actions, as a computer's root listing names them.
seats-signed-in = An account signed in
seats-left = An account left
seats-certified = Certify my key here
