# Notices the session shows the person: storage running out, the media codecs
# arriving, the session or the computer having stopped, an update waiting for a
# restart, and the machine's graphics having stopped.
battery-app-name = Power
battery-low-title = Battery low
battery-low-body = { $percent }% remaining. Consider plugging in.
battery-critical-title = Battery critically low
battery-critical-suspend-body = { $percent }% remaining. Suspending to protect your work.
battery-critical-plug-body = { $percent }% remaining. Please plug in now.

idle-inhibit-unnamed-app = An app

## What keeps this machine from suspending while someone uses it from another
## of their devices.
remote-use-hold = Remote use from another of your devices

storage-app-name = Storage
storage-volume-home = your files
storage-volume-system = the system
storage-low-title = Storage is almost full
storage-low-body = { $free } left for { $volume }. Delete large downloads or open Settings, Storage to free space.
storage-full-title = Storage is full
storage-full-body = Only { $free } left for { $volume }. New files and changes may not be saved until you free space. Everything else keeps working.

graphics-app-name = Graphics
graphics-stopped-title = Graphics stopped responding
graphics-stopped-body = Everything keeps working, but may be slower until you restart the computer.

codec-app-name = Video and audio formats
codec-hardware-ready = Hardware H.264/HEVC is ready
codec-installed = More video and audio formats are on
codec-installed-mesa-waiting = { $applies } Hardware H.264/HEVC follows once a matching driver is published.
codec-failed = More video and audio formats could not be turned on
codec-failed-retry = Open Settings, Updates, to try again.
codec-failed-with-error = { $error } Open Settings, Updates, to try again.

crash-notice-app-name = Desktop
crash-notice-title = The desktop restarted
crash-notice-froze-body = It stopped responding at { $time }. A record of what happened stays on this computer.
crash-notice-crashed-body = It stopped unexpectedly at { $time }. A record of what happened stays on this computer.
crash-notice-stopped-title = The computer restarted
crash-notice-stopped-body = It stopped unexpectedly before this start. A record of what happened stays on this computer.

system-update-app-name = Updates
system-update-restart-title = Restart to finish updating
system-update-restart-body = Your windows come back after the restart.
power-restart = Restart

## What this machine could not read of its own update, so it did not act on
## it. The next look reads it again.
update-state-unreadable-title = The update state could not be read
update-state-unreadable-body = This computer could not read how far its last update got, so it did not act on it. It looks again by itself; Settings, Updates shows what it knows.

## A deployment this computer tried did not start within its tries and the
## boot loader went back to the version that worked before: said once, with
## where the failed tries left what they found.
system-update-went-back-title = Went back to the version before
system-update-went-back-body = { $what } did not start after { $tries } tries, so this computer went back to { $running }. What the failed tries left behind: { $summary }, and tilo status.
system-update-went-back-unnamed = The new version
system-update-went-back-several-tries = several
system-update-went-back-runs-before = the version before

## The list of the person's own workspaces that this computer kept could not
## be read: it starts the list again instead of showing nothing.
workspaces-app-name = Workspaces
workspaces-unreadable-title = Your workspaces could not be read
workspaces-unreadable-body = The list of your own workspaces that this computer kept could not be read, so it starts the list again. Nothing was removed: your workspaces are still on this computer and on your other devices.

## The desk this device saved could not be opened: the person's own desk is
## still on disk and this session starts empty, so it is never written over.
desk-not-opened-app-name = Desktop
desk-not-opened-newer-title = Your desk was not opened
desk-not-opened-newer-detail = { $path } was saved by a newer version of Tilo (layout { $found }; this one understands { $understood }). It has not been changed: this session starts on an empty desk and keeps nothing, so it cannot be written over.
desk-not-opened-unreadable-title = Your desk could not be read
desk-not-opened-unreadable-detail = { $path } is there but could not be read ({ $error }). It has not been changed: this session starts on an empty desk and keeps nothing, so it cannot be written over.

## The desk from the version before this one, which was not brought over: it
## is still on disk, untouched, and this session has its own desk instead.
desk-not-opened-old-newer-title = Your old desk was not brought over
desk-not-opened-old-newer-detail = { $path } is your desk from the version before this one, saved by a newer one (layout { $found }; this one understands { $understood }). It has not been changed and is still on this machine: this session starts on an empty desk.
desk-not-opened-old-unreadable-title = Your old desk could not be read
desk-not-opened-old-unreadable-detail = { $path } is your desk from the version before this one, and it could not be read ({ $error }). It has not been changed and is still on this machine: this session starts on an empty desk.

## A window this device left open whose app did not come back.
desk-window-not-installed = { $app } is not installed on this machine
desk-window-would-not-start = { $app } could not be started: { $why }
desk-window-did-not-open = { $app } started but did not open a window

## A window the person's record kept whose saved state this version cannot
## read: its tab says why the window is not here instead of the place being
## dropped.
## The "Always allow" answers this computer kept for the agents at its door,
## which could not be read: each agent whose answer it was is asked again.
agent-bindings-app-name = Agents
agent-bindings-lost-title = { $agent } is asked again
agent-bindings-lost-body = This computer could not read that you let { $agent } work here, so it asks you again the next time { $agent } connects. Nothing else was removed.
agent-bindings-unreadable-title = Your answers about agents could not be read
agent-bindings-unreadable-body = This computer could not read which agents you let work here, so it asks you again the next time each one connects. Nothing else was removed.

## The registry of the programs the person downloaded does not read back: none
## of their apps is listed until it does, and none is gone.
refused-write-app = Changes
refused-write-title = { $device } will not take your change
refused-write-body = Your change stays on the devices that took it: { $reason }.

## Why a device or a workspace did not take a change, as the rest of the
## sentence before it.
refusal-reason-revoked = the device's access was revoked
refusal-reason-expired = the device's certificate expired
refusal-reason-not-certified = the device is not a member here
refusal-reason-forbidden = it may not change this
refusal-reason-malformed = the change could not be read
refusal-reason-waiting = it waits for earlier changes
refusal-reason-conflict = another change holds its place
refusal-reason-full = there is no room left there
refusal-reason-other = the reason is not known

# What a person changed on a workspace's shared desk did not arrive there: the
# change stays on this device.
desk-not-shared-app = Shared desk
desk-not-shared-title = { $workspace } did not take your window arrangement
desk-not-shared-body = It stays on this device: { $reason }.

# A collaborator's device speaks another Tilo protocol version: the two work
# together again once one of them updates.
version-mismatch-updating-title = { $device } runs a newer Tilo
version-mismatch-updating-body = This device updates and restarts onto it once your screen has been locked for a while.
version-mismatch-needs-update-title = { $device } needs to update
version-mismatch-needs-update-body = Update Tilo there and the two work together again.
version-mismatch-unknown-updating-title = Another device runs a newer Tilo
version-mismatch-unknown-updating-body = This device updates and restarts onto it once your screen has been locked for a while.
version-mismatch-unknown-needs-update-title = Another device needs to update
version-mismatch-unknown-needs-update-body = Update Tilo there and the two work together again.

# What a person chose in search, on a card or on a notification did not
# happen, and the row they chose it on is gone: the notice says what they
# asked for and why it did not happen.
action-failed-app = Tilo
action-failed-title = { $what } could not be done
action-failed-body = { $reason }
action-failed-that-row = That search row
action-failed-that-notification = What that notification opens
action-failed-that-app = That app
action-failed-no-workspace = This session has no workspace open
action-failed-that-settings = Settings
action-failed-that-change = That change
action-failed-that-agent = Taking that agent's access back
downloaded-apps-app-name = Downloaded apps
downloaded-apps-unreadable-title = Your downloaded apps could not be read
downloaded-apps-unreadable-body = This computer could not read the list of programs you added to your apps, so they are not listed now. Nothing was removed: they are still on this computer.

## A display tool tried to put back a layout the person had just reverted.
display-tool-reapply-refused = { $tool } tried to reapply a display layout you reverted
display-tool-reapply-refused-unknown = A display tool tried to reapply a display layout you reverted

## The session's own resources: the input devices, the screen, the night light,
## the magnifier, the displays, the clipboard and the questions about the
## person's apps.
session-input = Input devices
session-toggle-touchpad = Turn the touchpad on or off
session-param-touchpad-on = Whether the touchpad is on; leave out to switch it
session-next-input-source = Type in the next keyboard input source
session-screen = Screen
session-lock = Lock the screen
session-log-out = Log out
session-power-menu = Show the power menu
session-night-light = Night light
session-night-light-toggle = Turn the night light on or off
session-param-night-light-on = Whether the night light is on; leave out to switch it
session-magnifier = Magnifier
session-magnifier-toggle = Turn the magnifier on or off
session-magnifier-zoom-in = Magnify more
session-magnifier-zoom-out = Magnify less
session-displays = Displays
wireless-unavailable = Wireless displays aren't available right now
wireless-unavailable-detail = Try again in a moment.
wireless-search-failed = Couldn't look for wireless displays
wireless-search-failed-detail = Check that Wi-Fi is on, then try again.
wireless-connect-failed = Couldn't connect
wireless-connect-failed-detail = Check that the display is on and nearby, then try again.
wireless-lost = The display disconnected
wireless-lost-detail = Connect again when it's back.
wireless-stop-failed = Couldn't stop sharing
wireless-stop-failed-detail = Try again.
session-displays-next = Next display arrangement
session-displays-set = Arrange the displays
session-displays-open-sheet = Choose where the desktop appears
session-displays-close-sheet = Put the display sheet away
session-display-builtin = Built-in display
session-display-external = External display
session-clipboard = Clipboard
session-devices = Devices
session-devices-press = Press one of the person's devices
session-bluetooth-app = Bluetooth
session-bluetooth-moved = { $accessory } moved
session-bluetooth-moved-body = It works on { $device } now.
session-bluetooth-moving = Moving { $accessory }
session-bluetooth-moving-body = It will work on { $device }.
session-bluetooth-no-identity = This session is not one of the person's devices.
session-bluetooth-not-connected = It is not connected here.
session-bluetooth-not-connected-there = It could not connect on { $device }.
session-bluetooth-not-paired-there = { $device } does not hold its pairing.
session-bluetooth-not-released = This device could not let go of it.
session-bluetooth-cancelled = { $device } gave the move up.
session-bluetooth-stayed = { $accessory } stayed here
session-bluetooth-taken = { $accessory } is here now
session-bluetooth-taken-body = { $device } moved it.
session-bluetooth-target-away = The other device is not online.
session-bluetooth-target-unknown = The other device is not in the person's devices.
session-bluetooth-timed-out = The move took too long.
session-bluetooth-unknown = That is not a Bluetooth address.
session-notifications = Notification centre
session-notifications-dismiss = Dismiss the notification
session-notifications-act = Run the notification's action
session-notifications-clear = Clear the notification centre
session-notifications-undo-clear = Bring back the cleared notifications
session-notifications-open = Open the notification
## A notification another of the person's devices projected here: the app it
## came from and the device that showed it first.
notification-from-device = { $app } from { $device }
notification-from-device-only = From { $device }
## The person asked to report a problem and this computer has nowhere to
## send one: the report box says so where they asked, rather than opening
## on an address that would never carry it.
session-feedback-nowhere = Report a problem
session-feedback-nowhere-body = This computer is not set up to send reports about Tilo.
session-apps = Apps
session-apps-uninstall = Uninstall an app
session-param-app = The app's name

## A timer Clocks ran in the background has come due.
clock-due-title = Clocks
clock-due-body = Time is up

## A drive was plugged in and is ready to open or eject.
drives-app-name = Drives
drives-medium-title = { $name } ({ $size })
drives-unnamed = Removable drive
drives-body = Plugged in. Open it to see what is on it, or eject it before you pull it out.
drives-open = Open
drives-eject = Eject

## What came of uninstalling an app, or taking it off the person's apps.
app-list-app-name = Apps
app-list-gone-title = App not found
app-list-gone-body = It's no longer in your apps.

lock-screen-failed-title = Lock screen did not start
lock-screen-failed-body = Tilo draws the lock itself: the same password unlocks this computer.
app-list-uninstalled = Uninstalled { $name }
app-list-uninstalled-file = Its file is in the Trash.
app-list-uninstalled-files = All its copies are in the Trash.
app-list-removed = Removed { $name } from your apps
app-list-add-failed = Couldn't add { $name } to your apps
app-list-locate-failed = Couldn't find { $name }
app-list-remove-failed = Couldn't remove { $name } from your apps
app-list-uninstall-failed = Couldn't uninstall { $name }

## What came of asking a sleeping device to wake up.
device-wake-app-name = Devices
device-wake-rejoined = { $device } is awake
device-wake-not-rejoined = { $device } did not wake up
device-wake-not-rejoined-body = It got a wake-up signal but has not come back. It may be off or out of power.
device-wake-failed = { $device } could not be woken
device-wake-failed-body = No wake-up signal could be sent on its network.

## What a screen reader says about a pointer: whose it is, and the
## window they are over.
people-pointer-where = { $who } is in { $where }


## What the status cluster says of the machine in a line, for a reader
## that hears it rather than sees its glyphs.
status-battery = Battery { $percent }%
status-battery-charging = Battery { $percent }%, charging
status-muted = Muted
status-offline = Offline
status-volume = Volume { $percent }%
status-wifi = Wi-Fi
status-wifi-named = Wi-Fi: { $name }
status-wired = Wired
status-wired-named = Wired: { $name }

## The desk waits while this device's certificate is outside its validity at
## the clock's reading: the identity on disk is intact, and the screen says
## what the clock must reach before the desk opens by itself.
identity-clock-wait-title = Waiting for the clock
identity-clock-wait-body = Your Tilo identity is safe. The desktop opens once this computer's date and time are right.
identity-clock-wait-not-yet-valid = Your Tilo identity is safe. Its certificate starts at { $valid }, and this computer's clock reads { $now }. The desktop opens by itself once the clock passes that, or you set the date and time.
identity-clock-wait-expired = Your Tilo identity is safe. Its certificate ran until { $until }, and this computer's clock reads { $now }. The desktop opens by itself once the date and time are set right.

## what the readout calls the keyboard, whose backlight it just changed.
osd-keyboard-name = Keyboard

## What a paste of copied files into a folder did.
paste-app-name = Files
paste-refused-title = Nothing was pasted
paste-nothing-to-paste = Every name was taken, or the files are already in that folder.
paste-not-a-folder = { $path } is not a folder.
paste-stopped = { $reason }
paste-failed-title = Some files were not pasted
paste-failed-body = { $failed } of { $total } did not land. { $name }: { $reason }
paste-undone-title = The last paste did not finish
paste-undone-body = { $landed } of { $total } had already landed. Those are kept; the rest is where it was.
paste-undone-trouble = { $landed } of { $total } had already landed. Those are kept; the rest could not be put back yet: { $reason }

## What the desk's own rows say: the question on the person's avatar, a running
## application's tray item, and what the roster is called to assistive technology.
session-desk-answer-ask = Answer
session-desk-ask-later = Ask me later
session-desk-open = Open
session-desk-stop-agent = Stop
session-people = People

## What a person's private conversations are called, and the one action they offer.
## What the lock screen is called while it is up.
session-lock-screen = Lock screen

## What an agent's programs are, as the action door offers them: a task agent
## writes one program at a time as a step of its task's run.

## What the person's own devices answered when one of them asked to be
## unlocked after a reboot, as their activity history lists it: the package
## the rows stand under, and what an answer says.
unlock-history-app = Disk unlock
unlock-approved = Approved unlocking { $item }
unlock-denied = Declined to unlock { $item }
unlock-history-unknown-device = a device this computer cannot name

## What a window moved to another workspace asks, what it says when the app did
## not start where the person put it, and what it says when the record the window
## shows could not go along.
window-move-app-name = Workspaces
window-move-ask-title = Move { $app } to { $workspace }?
window-move-ask-what = The app starts again there, with the access that workspace gives it, and opens the record that workspace keeps.
window-move-ask-carries = The app starts again there, with the access that workspace gives it, and takes what the window shows along, edits and all. What it leaves behind says where it went.
window-move-ask-they-see = Everyone in { $workspace } can see the window and what it opens.
window-move-ask-you-leave = In { $workspace } it stops running, and what that workspace keeps stays there.
window-move-ask-carry = Move it
window-move-ask-stay = Leave it
window-move-failed-title = { $app } did not start in { $workspace }
window-move-failed-body = The window stayed in { $workspace }.
window-move-record-failed-title = { $app } could not take what it shows to { $workspace }
window-move-record-failed-body = The window stayed in { $workspace }: { $reason }.
window-move-reason-busy = the window's app still has it open
window-move-reason-record = its record cannot be moved to that workspace
window-move-reason-storage = this computer could not keep it there
window-move-reason-no-window = the app opened no window there
window-move-reason-no-start = the app did not start there
window-move-bringing-edits = The window comes back with what was written after it moved.

## The person's own card in the top bar: what it offers about who they are.
profile-action-identities = Your identities
profile-action-backup = Back up your identity
profile-action-pair-device = Pair another device
profile-name-not-saved = Your name was not saved

## A capture, while the capture studio's bar is down: the card the floating
## layer and the bar's pill show, and what its controls do.
capture-recording-title = Screen recording
capture-recording-stop = Stop
capture-recording-pause = Pause
capture-recording-resume = Resume
capture-replay-title = Replay buffer
capture-replay-save = Save
capture-replay-stop = Stop
capture-shot-title = Screenshot
capture-shot-copy = Copy
capture-shot-open = Open
capture-shot-reveal = Show in folder
capture-shot-dismiss = Done
capture-failed-not-saved = Couldn't save the screenshot
capture-failed-not-saved-permission = Tilo can't write to { $folder }
capture-failed-not-saved-no-room = there's no room left in { $folder }
capture-failed-not-saved-read-only = { $folder } is read-only
capture-failed-not-saved-folder = there's no folder at { $folder }
capture-failed-not-saved-other = the system wouldn't let Tilo write to { $folder }
capture-failed-copy-instead = Copy instead
capture-failed-dismiss = Done
capture-target-display = The whole desktop
capture-target-window = A window
capture-target-region = An area of the desktop

# The agent the session makes in a workspace that keeps none, when a person
# asks from the canvas (shell.md §8.1).
session-agent-name = Assistant
# The conversation a person asks in, made with Chat's own actions on the
# first ask (shell.md §8.1).
session-ask-conversation = Assistant

desk-style-failed-app-name = Desk
desk-style-failed-title = Your desk style could not be shown
desk-style-failed-body = { $style } could not be shown, so the desk shows Strips. Your choice is kept and the style is shown again once it is fixed. ({ $reason })

## Someone mentioned the person in a record of the workspace.
mention-notice-title = { $name } mentioned you in ‘{ $place }’
mention-notice-channel = { $name } mentioned you in { $channel }
mention-notice = { $name } mentioned you
mention-notice-someone = Someone
mention-notice-open = Open
