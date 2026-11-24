## The lock screen's power actions

power-suspend = Suspend
power-restart = Restart
power-shut-down = Shut down

## Display change

display-confirm-keep = Keep
display-confirm-revert = Revert
display-confirm-countdown = Reverting in { $seconds } s
display-confirm-applying = Applying…
display-confirm-answer-pending = Saving the display answer…
display-confirm-tool-title = Keep the display change from { $tool }?
display-confirm-tool-unknown-title = Keep this display change?
display-confirm-always = Keep, and don't ask again for { $tool }
display-confirm-a11y-description = A display tool changed a screen you're using, or picked a mode it hasn't shown before, which can leave it blank. Enter keeps the change; Escape, or waiting, restores the previous layout.

## Dictation

dictation-title = Dictation
dictation-ask-title = Dictate into this field?
dictation-ask-body = The microphone listens once, only until you stop, and the audio stays on this device. Nothing is recorded before you allow it.
dictation-ask-caption = Your words go into the field you were in.
dictation-allow = Allow
dictation-deny = Not now
dictation-preparing = Checking the focused editable field…
dictation-loading = Loading local recognition. The microphone is off.
dictation-listening = Microphone on. Speak, then finish to insert.
dictation-live-preview = Preview: { $text }
dictation-stopping-microphone = Stopping microphone…
dictation-finishing = Microphone off. Finishing your text…
dictation-stopping = Stopping dictation…
dictation-finish = Insert
dictation-cancel = Cancel
dictation-recovery-title = The field took no words
dictation-microphone-off = Microphone off
dictation-recovery-too-long = The text is too long to insert in one step. It is kept privately for 10 minutes. Copy exports all of it to your clipboard; discard removes it.
dictation-recovery-vocabulary = Your personal dictionary changed. Start a new session to use the updated words. What was heard is kept privately for 10 minutes; copy exports it, discard removes it.
dictation-recovery-kept = Kept privately for 10 minutes.
dictation-copied = Copied. Paste it into the field you choose; the private draft stays until you discard it or 10 minutes pass.
dictation-copy-failed = The clipboard is unavailable. Your transcript is still kept privately; try copying again or discard it.
dictation-copy = Copy
dictation-discard = Discard
dictation-unavailable-title = Dictation isn't available
dictation-unavailable = Local dictation is unavailable. No text was inserted.
dictation-editable-required = Focus an editable field. Passwords cannot receive dictation.
dictation-unsupported-field = Dictation is not supported in terminal fields yet. Focus another editable field.
dictation-busy = The previous dictation is still stopping. Try again shortly.
dictation-target-changed = The field changed. Dictation was canceled without inserting text.
dictation-timeout = Dictation timed out. No text was inserted.
dictation-model-required = Install an approved local speech model before using dictation.
dictation-language-unavailable = This speech model does not support the requested language.
dictation-microphone-unavailable = Microphone access is unavailable. Check your input device.
dictation-vocabulary-unavailable = Your personal dictionary changed or became unavailable. Dictation stopped; start a new session.
dictation-audio-interrupted = Microphone audio was interrupted. No text was inserted.
dictation-close = Close
dictation-open-settings = Set up

## Shortcut guide

guide-title = Keyboard shortcuts
guide-reopen-hint = Navigation shortcuts can be changed here
guide-done = Done
guide-press-new-shortcut = Press a new shortcut for “{ $action }”
guide-listening-cancel = Cancel
guide-rebind-needs-modifier = Add Super, Ctrl, Alt or Shift to printable keys.
guide-rebind-in-use = Already used for “{ $action }”. Press another shortcut.
guide-rebind-reserved = F1 and Print Screen are reserved by the desktop.
guide-rebind-unsupported = That shortcut is not supported. Press another shortcut.
guide-rebind-taken-by-action = Already used to run an action. Press another shortcut.
guide-rebind-save-failed = Could not save that shortcut. Press another shortcut.
guide-reset-in-use = A default is already used for “{ $action }”. Change that shortcut first.
guide-reset-taken-by-action = A default now runs an action. Change that shortcut first.
guide-reset-invalid = Could not restore the default shortcuts.
guide-reset-save-failed = Could not restore the defaults.
guide-navigation-heading = Edit navigation shortcuts
guide-essentials-heading = Desktop essentials
guide-group-windows = Windows and workspaces
guide-group-search = Search and system
guide-group-capture = Capture and display
guide-pointer = Pointer: click a window to focus it, drag tabs or groups to reorder them, drag an edge to resize
guide-controller-intro = Move with the stick or D-pad. Hold { $guide } to arrange windows. In a fullscreen game only { $guide } chords reach the desktop.
guide-row-move-focus = Move focus
guide-row-select = Select
guide-row-back = Back
guide-row-app-actions = Actions for this app
guide-row-search = Search
guide-row-tabs = Tabs
guide-row-workspaces = Workspaces
guide-overview = Overview
guide-row-this-guide = This guide
guide-row-windows = Windows
guide-row-strips = Strips
guide-row-move-window = Move window
guide-row-quick-settings = Quick settings
guide-touch-intro = Three fingers move along windows and between strips; four fingers switch workspaces.
guide-touch-focus-next-window = Focus next window
guide-touch-focus-previous-window = Focus previous window
guide-touch-strip-above = Strip above
guide-touch-strip-below = Strip below
guide-touch-previous-workspace = Previous workspace
guide-touch-next-workspace = Next workspace
guide-touch-open-overview = Open overview
guide-touch-close-overview = Close overview
guide-reset = Reset to default

## What the keyboard's navigation rows move, and what else to know: windows,
## tabs, strips and workspaces read as the controller rows above name them
guide-row-move-tab = Move tab
guide-row-maximize = Maximize or restore
guide-row-move-group = Move group
guide-row-resize = Resize
guide-row-move-group-strip = Move group to strip
guide-unassigned = Unassigned
guide-unassigned-inline = unassigned
guide-strip-jump = Go straight to a strip with { $first } … { $last }; strip 1 is the top one
guide-app-tab-chords = { $next }, { $previous } and { $reopen } go to the app first; the desktop’s tab shortcuts here always work
guide-browser-keys = In a browser tab, { $close }, { $new }, { $window } and { $switch } belong to the browser; use the keys shown here, or install Tilo as an app and they work in Tilo
guide-published-heading = Your shortcuts

## The catalogue's shortcut names, which the guide lists
shortcut-name-focus-previous-window = Focus previous window
shortcut-name-focus-next-window = Focus next window
shortcut-name-previous-tab = Previous tab
shortcut-name-next-tab = Next tab
shortcut-name-attach-tab-left = Move tab into group on left
shortcut-name-attach-tab-right = Move tab into group on right
shortcut-name-maximize-window = Maximize window
shortcut-name-restore-window = Restore window
shortcut-name-previous-strip = Strip above
shortcut-name-next-strip = Strip below
shortcut-name-previous-workspace = Previous workspace
shortcut-name-next-workspace = Next workspace
shortcut-name-move-group-left = Move group left
shortcut-name-move-group-right = Move group right
shortcut-name-resize-narrower = Make window narrower
shortcut-name-resize-wider = Make window wider
shortcut-name-move-group-to-previous-strip = Move group to strip above
shortcut-name-move-group-to-next-strip = Move group to strip below
shortcut-name-reopen-closed-tab = Reopen closed tab
shortcut-name-search = Search everywhere
shortcut-name-emoji-picker = Emoji
shortcut-name-workspace-overview = Overview
shortcut-name-quick-settings = Quick Settings
shortcut-name-lock = Lock screen
shortcut-name-toggle-fullscreen = Full screen
shortcut-name-detach-tab = Move tab to new group
shortcut-name-open-files = Open Files
shortcut-name-notifications = Notifications
shortcut-name-clipboard-history = Clipboard history
shortcut-name-ask-agent = Ask agent about selection
shortcut-name-say-something = Say something here
shortcut-name-report-problem = Report a problem
peek-apps = Apps
peek-agent-search = Agent and search
shortcut-switch-windows = Switch windows
shortcut-close-tab = Close tab
shortcut-new-window = New window
shortcut-duplicate-tab = New tab of this app
shortcut-strip-switcher = Strip switcher
shortcut-screenshot = Screenshot
shortcut-display-layout = Display layout
shortcut-capture-screenshot = Capture screenshot
shortcut-open-terminal = Open terminal
shortcut-picture-in-picture = Picture in picture
shortcut-save-recent-replay = Save recent replay

## Public workspaces

directory-title = Join a workspace
directory-search = Search public workspaces, or paste a link or code
directory-link-or-code = Join with a link or code
directory-loading = Looking for public workspaces…
directory-empty-title = No public workspaces yet
directory-empty = Type a site like tilo.sh and press Enter to see its workspaces.
directory-no-match-title = Nothing matches “{ $query }”
directory-no-match = Press Enter to look it up as a site.
directory-failed-title = Couldn't read the workspaces
directory-error-no-workspace = that site has no public workspaces to show
directory-error-unreachable = the site could not be reached
directory-error-unreadable = the site answered with something that is not a directory
directory-error-status = the site would not share its workspaces just now
directory-error-invitation-gone = This invite link no longer works. Ask whoever sent it for a new one.
directory-error-invitation-broken = that link does not open the invitation it names
directory-here-more = +{ $count }
directory-sites-unread = Some sites could not be read, so this list is short: { $error }
directory-listing-gone = That workspace is no longer listed. Read the site again.
directory-join = Join
directory-open = Open
directory-joining = Joining…
directory-join-named = Join { $name }
directory-joining-named = Joining { $name }
directory-open-named = Open { $name }
directory-try-again = Try again
directory-community-tag = { $host }'s own
directory-already-joining = You're already joining { $name }. It opens as soon as it's ready.
directory-join-failed = Couldn't join { $name }: { $error }
directory-code-failed = Couldn't join with that link or code: { $error }
directory-join-failed-line = Didn't join: { $error }
directory-here-now = { $count } here now
directory-active-today = Active today
directory-active-week = Active this week
directory-quiet = Quiet lately
directory-members = { $count ->
    [one] 1 member
   *[other] { $count } members
}
directory-joins-as = You join as { $role }
directory-role-viewer = a viewer
directory-role-commenter = a commenter
directory-role-editor = an editor
directory-role-moderator = a moderator
directory-role-owner = the owner

## Display sheet
display-sheet-title = Display
display-sheet-a11y-description = Up and Down move between choices, Enter applies the highlighted one, Super+P moves to the next layout, Escape closes.
display-mode-extend = Extend
display-mode-extend-detail = One desktop across every display
display-mode-mirror = Mirror
display-mode-mirror-detail = The same picture on every display
display-mode-builtin-only = Only this screen
display-mode-builtin-only-detail = External displays turn off
display-mode-external-only = Only external
display-mode-external-only-detail = The built-in screen turns off
display-sheet-one-display = No other display connected
display-sheet-one-display-detail = Connect one with a cable, or choose a wireless display.
display-confirm-title = Keep these display settings?
display-rejected = Couldn't apply this layout: { $reason }
wireless-heading = Wireless displays
wireless-share-audio = Share audio
wireless-stream = Stream
wireless-stream-auto = Auto
wireless-stream-fixed = Fixed
wireless-stream-fps = { $fps } fps
wireless-credential-label = Receiver PIN or password
wireless-credential-placeholder = PIN or receiver password
wireless-credential-hint = Leave blank unless the receiver asks. Enter connects.
wireless-scanning = Looking for displays…
wireless-scanning-detail = Keep the receiver awake and on the same network.
wireless-none = No displays found
wireless-none-detail = Check that Wi-Fi is on and the display is ready to cast.
wireless-search-again = Search again
wireless-try-again = Try again
wireless-device-detail = { $protocol }, { $detail }
wireless-connected = Connected
wireless-connecting = Connecting…
wireless-connect = Connect
wireless-stop = Stop
wireless-wait = Wait
wireless-protocols = Miracast, Google Cast, AirPlay and FCast
display-sheet-saving-layout = Saving display layout…

## The system sheets the shell asks with on an app's or a service's behalf
portal-secret-incomplete = Fill in what the question asks first.
portal-allow = Allow
portal-deny = Deny
portal-allow-once = Allow once
portal-allow-session = Allow for this session
portal-allow-always = Always allow

## Access to a capability
portal-access-title = { $app } wants to use { $what }
portal-access-detail = Nothing is shared until you allow it.
portal-access-screenshot = your screen, as a picture
portal-access-screencast = your screen
portal-access-remote-desktop = your screen and your input
portal-access-camera = your camera
portal-access-microphone = your microphone
portal-access-speaker = your speakers
portal-access-location = your location
portal-access-print = your printers
portal-access-usb = your USB devices
portal-access-input-capture = your keyboard and pointer
portal-access-global-shortcuts = global keyboard shortcuts
portal-access-notification = your notifications
portal-access-secret = your saved passwords
portal-access-host-shell = the device's own shell
portal-access-generic = { $capability }
portal-access-network = the internet
portal-network-hosts = It reaches only these sites. Everyone in this workspace sees this and can take it back.
portal-network-any = It may reach any site. Everyone in this workspace sees this and can take it back.
portal-network-site = Site

## The portal's other questions
portal-account-title = Share your identity with { $app }?
portal-account-detail = { $app } asks for your name and account identifier.
portal-email-title = Open the message { $app } prepared?
portal-email-detail = Check the message before it is handed on.
portal-email-to = To: { $addresses }
portal-email-cc = Cc: { $addresses }
portal-email-bcc = Bcc: { $addresses }
portal-email-subject = Subject: { $subject }
portal-email-attachments = Attachments: { $count }
portal-background-title = Let { $app } keep running?
portal-background-background = Run in the background
portal-background-running = Run while its window is open
portal-background-active = Run only while it is in front
portal-wallpaper-title = Set the wallpaper { $app } asks for?
portal-wallpaper-detail = { $app } changes { $where }.
portal-wallpaper-background = your desktop background
portal-wallpaper-lockscreen = your lock screen
portal-wallpaper-both = your desktop background and lock screen
portal-print-title = Let { $app } print “{ $title }”?
portal-print-detail = { $copies ->
    [one] 1 copy, to the printer you choose.
   *[other] { $copies } copies, to the printer you choose.
}
portal-screenshot-title = Let { $app } take a screenshot?
portal-screenshot-detail = It gets one picture of the screen as it is now.
portal-usb-title = Give { $app } USB devices?
portal-usb-detail = { $app } gets the ticked devices until it stops asking or one is unplugged.
portal-app-choice-title = Open with what?
portal-app-choice-detail = { $app } wants to open { $what }
portal-app-choice-detail-any = Choose the app that opens it
portal-launcher-title = Let { $app } add “{ $name }” to your apps?
portal-launcher-detail = The image it supplied has been checked.
portal-launcher-app-icon = The app's own image
portal-launcher-monogram-icon = Tilo monogram
portal-launcher-web-icon = Tilo web icon
portal-launcher-bad-name = Give the launcher a name, without control characters.
portal-shortcuts-title = Let { $app } set global shortcuts?
portal-shortcuts-detail = Choose a row, then press the keys. Backspace clears it.
portal-shortcut-reserved = Reserved by Tilo. Choose another.
portal-shortcut-taken = Used by { $app }. Choose another.
portal-share-title = Share your screen with { $app }?
portal-share-detail = { $app } sees what you share, and nothing else.
portal-remote-title = Let { $app } control this computer?
portal-remote-detail = { $app } sees the screen you share and drives the input you allow.
portal-remote-devices = It asks to control: { $devices }

## Administrator authentication
portal-auth-heading = Administrator access
portal-auth-ended = Authentication ended unexpectedly.
portal-auth-rejected = That password was not accepted.

## File chooser


## The lock screen.
lock-title = Locked
lock-unlock = Unlock
lock-password = Password
lock-caps-lock = Caps Lock is on
lock-checking = Checking…
lock-no-password = Type your password to unlock.
lock-wrong-password = That password did not unlock. Try again.
lock-unavailable = The password could not be checked. The screen stays locked.
lock-switch-layout = Switch to { $layout }
lock-on-screen-keyboard = On-screen keyboard
lock-play = Play
lock-pause = Pause
lock-next = Next song
lock-previous = Previous song
lock-battery = Battery
lock-battery-critical = Battery at { $percent }. Plug in to keep this machine running.
lock-notifications = { $count ->
    [one] 1 notification
   *[other] { $count } notifications
}
lock-suspend = Suspend
lock-restart = Restart
lock-shut-down = Shut down

## The questions the session shows on the portal sheet: apps' access, the
## administrator's, a network's secrets and a device pairing.
portal-connect = Connect
portal-remember-device = Remember on this device
portal-secret-required = Fill in every required field.
portal-secret-wifi-length = A Wi-Fi password is 8 to 63 characters, or 64 hexadecimal digits.
portal-sim-unlock-failed = The SIM could not be unlocked. Try again.
portal-sim-pin-wrong = That PIN is wrong.
portal-sim-puk-title = SIM locked
portal-sim-puk-lead = Too many wrong PINs locked the SIM for “{ $network }”. Enter the PUK from the SIM’s card holder and choose a new PIN.
portal-sim-puk-wrong = That PUK is wrong.
portal-network-this-network = this network
portal-network-password = Password
portal-network-key = Key
portal-network-user-name = User name
portal-network-private-key-password = Private key password
portal-network-anonymous-identity = Anonymous identity (optional)
portal-network-wifi-title = Wi-Fi password required
portal-network-enter-password = Enter the password for “{ $network }”.
portal-network-wired-sign-in-title = Wired network sign-in
portal-network-sign-in-title = Network sign-in
portal-network-sign-in-lead = “{ $network }” needs your user name and password.
portal-network-vpn-title = VPN sign-in
portal-network-vpn-lead = Enter the secrets for “{ $network }”.
portal-network-sim-pin-title = SIM PIN required
portal-network-sim-pin-lead = Enter the PIN to unlock the SIM for “{ $network }”.
portal-network-sim-pin = SIM PIN
portal-network-mobile-title = Mobile broadband password required
portal-network-dsl-title = DSL password required
portal-network-retry = The last attempt did not work. Check it and try again.
portal-network-wps = You can also press the WPS button on the router.
portal-network-cert-pinned = The network's certificate is checked against this connection's certificate authority.
portal-network-cert-pinned-domain = The network's certificate is checked against this connection's certificate authority. It must belong to { $domain }.
portal-network-cert-system = The network's certificate is checked against the system's trusted authorities.
portal-network-cert-system-domain = The network's certificate is checked against the system's trusted authorities. It must belong to { $domain }.
portal-network-cert-unchecked = The network's certificate is not checked. Only continue on a network you trust.
network-cannot-join-title = Couldn't join { $network }
network-cannot-join-body = Enter the password to connect.
network-cannot-join-action = Enter password
bluetooth-prompt-title-confirm-passkey = Does { $device } show this code?
bluetooth-prompt-title-display = Type this on { $device }
bluetooth-prompt-title-request-passkey = Enter the code shown on { $device }
bluetooth-prompt-title-request-pin = Enter the PIN for { $device }
bluetooth-prompt-title-confirm-pairing = Pair with { $device }?
bluetooth-prompt-title-authorize-service = Let { $device } connect?
bluetooth-prompt-explain-confirm-passkey = Compare it with the code on the other device. If they differ, something else is trying to pair, so say no.
bluetooth-prompt-explain-display = Enter this code on the other device to finish pairing.
bluetooth-prompt-explain-request = The code is shown on the device you are pairing. Many keyboards ask you to type it and press Enter.
bluetooth-prompt-explain-confirm-pairing = This device cannot show a code to compare, so there is nothing to check against. Only continue if you started this pairing yourself.
bluetooth-prompt-explain-authorize-service = A device you have already paired is asking to use a new service. Paired devices can send input and audio to this machine.
bluetooth-prompt-digits-entered = { $entered } of 6 digits entered
bluetooth-prompt-service = For { $service }
bluetooth-service-audio = audio
bluetooth-service-calls = calls
bluetooth-service-remote-control = media controls
bluetooth-service-input = input
bluetooth-service-contacts = contacts
bluetooth-service-messages = messages
bluetooth-service-files = file transfer
bluetooth-service-network = network sharing
bluetooth-prompt-cancel = Cancel
portal-allowed-title = Allowed for good
portal-allowed-detail = { $app } may now use { $what } without asking. Take it back to make Tilo ask again next time.
portal-revoke = Take back

## Adding a program the person downloaded to their apps, and uninstalling an app
add-app-title = Add { $name } to apps?
add-app-add = Add to apps
add-app-decline = Don't add
add-app-move = Move to Applications
add-app-move-detail = Keeps it in the Applications folder in your files
uninstall-title = Uninstall { $name }?
uninstall-action = Uninstall
uninstall-cancel = Cancel
uninstall-remove-data = Remove its data
uninstall-remove-data-detail = Your settings and saved files go to the Trash, where you can restore them.
uninstall-device-body = It's removed for everyone on this computer.
uninstall-files = { $count ->
    [one] Its file goes to the Trash, where you can restore it.
   *[other] All { $count } copies go to the Trash, where you can restore them.
}
uninstall-home-folder = Home
uninstall-documents-body = Your documents stay in your workspaces.
uninstall-from-apps-title = Remove { $name } from apps?
uninstall-from-apps-action = Remove from apps
uninstall-entry-body = It's part of the system, so it stays on this computer and no longer shows in your apps.

## The MCP server: a program of this login (an AI agent in a terminal) asks to
## work in a workspace as an agent member.
mcp-server-client = { $client } ({ $program })
mcp-server-client-in-app = { $who }, in { $app }
mcp-server-admission = Work in “{ $workspace }” as the agent { $agent }
mcp-server-admission-detail = It joins this workspace as an observer: it reads what the workspace holds and does the actions it is granted, and you can remove it in the workspace’s settings at any time.
mcp-server-no-workspace = There is no workspace here to work in.
mcp-server-client-in-environment = { $who }, in the environment “{ $environment }”
mcp-server-workspace-refused = You can’t let an agent work in that workspace from here.
mcp-server-environment-workspace = A program in an environment works in that environment’s workspace only.
mcp-server-workspace-gone = The workspace this program asked to work in is gone.
mcp-server-withdrawn = The request was withdrawn before anyone answered it.
mcp-server-declined = You didn’t let this program work here as an agent.
mcp-server-not-started = This agent’s work here could not start

## The shortcut catalogue on the action API: every desktop shortcut with its
## live and default chords, as the shortcut guide shows them.
keyboard-shortcuts-resource = Keyboard shortcuts
keyboard-shortcuts-list = List shortcuts
keyboard-shortcuts-list-description = Every desktop shortcut with its current and default keys.

## An agent asks its person something in its own words: a decision, or
## something only they know.
agent-ask-title = { $agent } asks you
agent-ask-send = Send
agent-ask-decline = Don't answer
agent-ask-not-now = Ask me later
agent-ask-secret-title = { $agent } needs a password from you
agent-ask-password = Password
agent-ask-sign-in = Sign in
agent-ask-notice-app = Agents
agent-ask-answer = Answer
agent-ask-pick = Pick an answer.

## The collaboration network's own questions: an agent on a workspace host
## asks its person to allow one action, and a device asks its sibling for the
## disk key it boots with.
network-device-unlock-title = Unlock { $device }?
network-device-unlock-detail = It is starting up and asks you for the half of its disk key that this device holds for it, for { $slot }. Compare the code above with the one on its boot screen.
network-device-enrol-title = Set up unlock for { $device }?
network-device-enrol-detail = It will unlock its disk at boot with this device's help.
network-device-enrol-unattended = It will then start without asking you again.

## What each sheet's resource is called, keyed by its kind with separators
## as dashes (`power.session` is `power-session`). The label a sheet declares
## is the English text here, so a desktop in another language names the
## resource it publishes in that language.
power-session = Power
lock-screen = Lock screen
dictation-session = Dictation
uninstall-question = Uninstall
add-app-question = Add to apps
shortcut-guide-guide = Keyboard shortcuts
workspace-directory-listing = Public workspaces
display-sheet-layouts = Displays
display-confirm-question = Display change
workspace-settings-lens = Workspace settings

# Deleting a workspace, asked wherever it was asked from: the confirm sheet
# and the workspace settings' own question say the same words.
workspace-delete-title = Delete { $name }?
workspace-delete-body = Everything in this workspace goes with it: its windows, its groups, its history. It disappears from every device that has it.
workspace-delete-confirm = Delete
workspace-delete-keep = Keep

# Inviting people: the link into the workspace in front, and the links that
# still work.
invite-title = Invite people
invite-title-workspace = Invite people to “{ $workspace }”
invite-description = Make a link that brings someone into this workspace.
invite-close = Close
invite-refusal = You can't invite people to this workspace. Its owner decides who joins.
invite-role-label = They join as
invite-for-label = This link is for
invite-anyone = Anyone with the link
invite-joined = { $names } joined.
invite-joined-someone = Someone joined.
invite-create = Create link
invite-create-another = Create another link
invite-making = Making the link…
invite-link-label = The link
invite-copy = Copy link
invite-code-alt = The invitation link as a code to scan
invite-lasting-minutes = { $minutes ->
    [one] Works for the next minute.
   *[other] Works for the next { $minutes } minutes.
}
invite-lasting-hours = { $hours ->
    [one] Works for the next hour.
   *[other] Works for the next { $hours } hours.
}
invite-for-anyone = For anyone with the link
invite-for-person = For { $name }
invite-for-someone = For a person this workspace knows
invite-pending-title = Links that still work
invite-revoke = Take back
invite-copied = Copied
invite-revoked = The link no longer works.
invite-no-answer = No link was made: the workspace did not answer.
invite-failed = No link was made: { $reason }
invite-revoke-failed = The link was not taken back: { $reason }

## What the window names
sheets-workspace-activity = { $people } here now
sheets-workspace-person = Person { $number }

## Actions

so-tilo-add-app-add = Add to your apps
so-tilo-add-app-decline = Do not ask about this file again
delete-workspace-question = Delete workspace
so-tilo-delete-workspace-answer-delete = Delete it and everything in it
so-tilo-delete-workspace-answer-keep = Keep it
so-tilo-workspace-directory-join-clear-note = Dismiss the notice
so-tilo-workspace-directory-join-done = Done
so-tilo-workspace-directory-join-join = Join a public workspace
so-tilo-workspace-directory-join-link-or-code = Join with a link or code
so-tilo-workspace-directory-join-look = Look up a site
so-tilo-workspace-directory-join-open = Open a workspace
so-tilo-workspace-directory-join-refresh = Refresh
so-tilo-display-confirm-answer-keep-and-trust = Keep, and don't ask again
so-tilo-display-sheet-choose-adaptive-quality = Adapt the quality
so-tilo-display-sheet-choose-builtin-only = Built-in screen only
so-tilo-display-sheet-choose-choose = Choose a wireless screen
so-tilo-display-sheet-choose-close = Close
so-tilo-display-sheet-choose-connect = Start streaming
so-tilo-display-sheet-choose-extend = Extend
so-tilo-display-sheet-choose-external-only = External screens only
so-tilo-display-sheet-choose-frame-rate = Frame rate
so-tilo-display-sheet-choose-include = Use another device's screen
so-tilo-display-sheet-choose-keep = Keep
so-tilo-display-sheet-choose-mirror = Mirror
so-tilo-display-sheet-choose-next-resolution = Next resolution
so-tilo-display-sheet-choose-revert = Revert
so-tilo-display-sheet-choose-search = Look again
so-tilo-display-sheet-choose-share-audio = Share the sound
so-tilo-display-sheet-choose-stop = Stop streaming
invite-links = Invitations
so-tilo-invite-link-clear-note = Dismiss the notice
so-tilo-invite-link-copy = Copy the link
so-tilo-invite-link-create = Make an invitation
so-tilo-invite-link-done = Done
so-tilo-invite-link-revoke = Take the invitation back
so-tilo-lock-screen-dissolved = The screen has gone
so-tilo-lock-screen-next-layout = Next keyboard layout
so-tilo-lock-screen-next-track = Play the next song
so-tilo-lock-screen-play-pause = Play or pause
so-tilo-lock-screen-previous-track = Play the previous song
so-tilo-lock-screen-show-keyboard = Show the on-screen keyboard
so-tilo-lock-screen-type-password = Type the password
so-tilo-lock-screen-unlock = Unlock
so-tilo-shortcut-guide-edit-listen = Change the shortcut
so-tilo-shortcut-guide-edit-reset = Reset the shortcuts
so-tilo-uninstall-cancel = Keep it
so-tilo-uninstall-confirm = Uninstall
so-tilo-workspace-settings-change-add-agent = Add an agent
so-tilo-workspace-settings-change-add-emoji = Add an emoji
so-tilo-workspace-settings-change-add-server = Have another server keep the workspace as well
so-tilo-workspace-settings-change-close = Close
so-tilo-workspace-settings-change-delete = Remove from this desktop
so-tilo-workspace-settings-change-delete-workspace = Delete the workspace
so-tilo-workspace-settings-change-dismiss-emoji-notice = Dismiss the notice
so-tilo-workspace-settings-change-dismiss-notice = Dismiss the notice
so-tilo-workspace-settings-change-end-review = Back to the agents
so-tilo-workspace-settings-change-grant = Grant a permission
so-tilo-workspace-settings-change-invite = Invite people
so-tilo-workspace-settings-change-move = Move the workspace
so-tilo-workspace-settings-change-next-personal-default = Open this kind for you
so-tilo-workspace-settings-change-next-workspace-default = Open this kind for everyone
so-tilo-workspace-settings-change-remove-agent = Remove the agent
so-tilo-workspace-settings-change-remove-emoji = Remove the emoji
so-tilo-workspace-settings-change-remove-member = Remove a member
so-tilo-workspace-settings-change-remove-server = Have a server keep the workspace no more
so-tilo-workspace-settings-change-rename = Rename the workspace
so-tilo-workspace-settings-change-reset-agent-app = Reset the app's answers
so-tilo-workspace-settings-change-review-activity = Review the activity
so-tilo-workspace-settings-change-review-agent = Review an agent
so-tilo-workspace-settings-change-revoke = Revoke a permission
so-tilo-workspace-settings-change-revoke-delegation = Take back your work from a server
so-tilo-workspace-settings-change-set-agent-answer = Answer for one kind
so-tilo-workspace-settings-change-set-agent-model = Choose the model
so-tilo-workspace-settings-change-set-agent-spend-limit = Set the daily limit
so-tilo-workspace-settings-change-set-role = Change a role
workspace-settings-document-access = Data access
workspace-settings-document-label = Applies to
workspace-settings-access-all-apps = All workspace apps
workspace-settings-access-app-default = { $app } defaults
workspace-settings-document-everyone = Everyone
workspace-settings-document-inherit = Inherit
workspace-settings-document-no-access = No access
workspace-settings-document-view = Can view
workspace-settings-document-edit = Can edit
workspace-settings-document-empty = There are no shared documents to manage.
workspace-settings-document-apply = Apply access rule
workspace-settings-custom-roles = Custom roles
workspace-settings-create-role = Create role
workspace-settings-role-name = Role name
workspace-settings-rename-role = Rename
workspace-settings-save-role = Save role
workspace-settings-delete-role = Delete this role?
workspace-settings-delete-role-detail = Its permissions and member assignments will be removed.
workspace-settings-keep-role = Keep role
workspace-settings-delete-role-confirm = Delete role
so-tilo-workspace-settings-change-save-custom-role = Create or rename a role
so-tilo-workspace-settings-change-set-custom-role = Assign or remove a member's role
so-tilo-workspace-settings-change-remove-custom-role = Delete a role and its rules
so-tilo-workspace-settings-change-set-document-access = Set document access
so-tilo-workspace-settings-change-show-tab = Show a tab
so-tilo-workspace-settings-change-toggle-device = Show on a device
## What a program's question shows on the asking view.
portal-auth-action = Action
portal-network-asker = Network
portal-sim-puk = PUK
portal-sim-new-pin = New PIN
portal-sim-new-pin-again = New PIN again
portal-sim-pins-differ = The two new PINs differ.
portal-sim-attempts-left = { $count ->
    [one] 1 attempt left
   *[other] { $count } attempts left
}
portal-launcher-name = Name
portal-files-title = Choose files for { $app }
portal-files-writable-folder = { $app } will be able to change what is in the folder you choose.
portal-usb-writable = { $device } (can be written to)
bluetooth-prompt-code = Code
network-device-unlock-code = Code on its screen
