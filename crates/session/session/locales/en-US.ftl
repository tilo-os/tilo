# Notices the session shows the person: storage running out, the media codecs
# arriving, the session or the computer having stopped, an update waiting for a
# restart, and the machine's graphics having stopped.
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
session-clipboard = Clipboard
session-devices = Devices
session-devices-press = Press one of the person's devices
session-notifications = Notification centre
session-notifications-dismiss = Dismiss the notification
session-notifications-act = Run the notification's action
session-notifications-clear = Clear the notification centre
session-notifications-undo-clear = Bring back the cleared notifications
session-notifications-open = Open the notification
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
