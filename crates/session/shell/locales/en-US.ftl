person-quick-accessible-name = Message person
person-quick-message = Message
person-quick-title = { $person }
person-quick-placeholder = Write a private message…
person-quick-editor-name = Private message draft
person-quick-send = Send private message
person-quick-expand = Open private conversation
person-quick-close = Close message draft
person-quick-unavailable = Private conversation source unavailable. Your draft is kept.

# Continuing on this desktop what you were just doing on another device.
continuity-continue = Continue { $app } from { $device }
continuity-continue-subtitle = Focus the tab you were just using on { $device }
continuity-bringing-record = Bringing its document over from another device…
record-not-kept-here = That document is not kept on this device.
record-too-large-to-bring = That document is too large to bring over.
record-offset-past-end = That part of the document does not exist.
continuity-dismiss-title = Dismiss “Continue { $app } from { $device }”
continuity-dismiss-subtitle = Hidden until you switch tabs on { $device } again
continuity-other-device = your other device

# Undoing a tab close.
tab-closed-toast = Closed { $title }
tab-closed-group-toast = Closed { $count } tabs
tab-closed-undo = Undo
workspace-deleted-toast = Deleted { $name }
workspace-deleted-undo = Undo

# Display layout sheet (Super+P)
display-sheet-title = Display
display-sheet-subtitle = Choose where your desktop appears. Super+P moves to the next choice.
display-sheet-a11y-description = Up and Down move between choices, Enter applies the highlighted one, Super+P moves to the next layout, Escape closes.
display-mode-extend = Extend
display-mode-extend-detail = One desktop across every display
display-mode-mirror = Mirror
display-mode-mirror-detail = The same picture on every display
display-mode-builtin-only = Only this screen
display-mode-builtin-only-detail = External displays turn off
display-mode-external-only = Only external
display-mode-external-only-detail = The built-in screen turns off
display-mode-current = Current
display-mode-unavailable-single = Connect another display to use this
display-mode-unavailable-builtin = This device has no built-in screen
display-mode-unavailable-mirror = These displays share no resolution
display-confirm-title = Keep these display settings?
display-confirm-countdown = Reverting in { $seconds } s
display-confirm-applying = Applying…
display-confirm-keep = Keep
display-confirm-revert = Revert
display-rejected = Couldn't apply this layout: { $reason }

# Keep or revert a risky change a display tool (kanshi, wlr-randr) made:
# one that could leave a screen blank or turn off the one in use
display-confirm-tool-title = Keep the display change from { $tool }?
display-confirm-tool-unknown-title = Keep this display change?
display-confirm-always = Keep, and don't ask again for { $tool }
display-confirm-a11y-description = A display tool changed a screen you're using, or picked a mode it hasn't shown before, which can leave it blank. Enter keeps the change; Escape, or waiting, restores the previous layout.
display-tool-reapply-refused = { $tool } tried to reapply a display layout you reverted
display-tool-reapply-refused-unknown = A display tool tried to reapply a display layout you reverted

# Wireless receivers, the lower section of the same sheet
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
wireless-scan-failed = Couldn't scan
wireless-none = No displays found
wireless-none-detail = Try Refresh, or check Wi-Fi and the receiver's casting mode.
wireless-connected = Connected
wireless-connecting = Connecting…
wireless-connect = Connect
wireless-stop = Stop
wireless-wait = Wait
wireless-protocols = Miracast, Google Cast, AirPlay and FCast

# Identify displays: the card painted on each display
display-identify-builtin = Built-in display
display-identify-unnamed = Display { $number }

# The overview's row of the current workspace's windows.
overview-windows-title = Windows in this workspace
overview-windows-description = Each strip of this workspace, top to bottom, with its windows in order. The arrow keys move between them and Enter goes to one. F2 renames the strip a window is on, and Alt with Up or Down moves that strip. Click or tap a window to go there, middle-click one to close it, or drag it onto another workspace to move it there.
overview-window-move-failed = Can't move this window
overview-strips-title = Strips in this workspace
overview-strip-empty = No windows
overview-strip-rename = Rename
overview-strip-move-up = Move up
overview-strip-move-down = Move down
overview-strip-move-failed = Can't move this strip
overview-strip-rename-failed = Can't rename this strip

# Fixed shell chords, as the shortcut guide and the keyboard map name them
shortcut-screenshot = Screenshot
shortcut-switch-windows = Switch windows
shortcut-switch-windows-backwards = Switch windows backwards
shortcut-switch-app-windows = Switch windows of the app
shortcut-close-tab = Close tab
shortcut-change-workspace = Change workspace
shortcut-new-window = New window
shortcut-duplicate-tab = New tab of this app
shortcut-open-terminal = Open terminal
shortcut-strip-switcher = Strip switcher
shortcut-display-layout = Display layout
shortcut-capture-screenshot = Capture screenshot
shortcut-picture-in-picture = Picture in picture
shortcut-save-recent-replay = Save recent replay
shortcut-workspace-history = Workspace history
shortcut-explore-workspace-history = Explore workspace history
shortcut-guide = Keyboard shortcuts
shortcut-stop-remote-input = Stop all remote input
shortcut-toggle-mouse-keys = Toggle mouse keys
shortcut-vim-navigation = Vim navigation
shortcut-dismiss = Dismiss

# What each changeable desktop shortcut is called
shortcut-name-search = Search everywhere
shortcut-name-workspace-overview = Overview
shortcut-name-lock = Lock screen
shortcut-name-quick-settings = Quick Settings
shortcut-name-shortcut-help = Keyboard shortcuts
shortcut-name-focus-previous-window = Focus previous window
shortcut-name-focus-next-window = Focus next window
shortcut-name-previous-tab = Previous tab
shortcut-name-next-tab = Next tab
shortcut-name-previous-strip = Strip above
shortcut-name-next-strip = Strip below
shortcut-name-previous-workspace = Previous workspace
shortcut-name-next-workspace = Next workspace
shortcut-name-toggle-maximize = Maximize or restore window
shortcut-name-maximize-window = Maximize window
shortcut-name-restore-window = Restore window
shortcut-name-detach-tab = Move tab to new group
shortcut-name-attach-tab-left = Move tab into group on left
shortcut-name-attach-tab-right = Move tab into group on right
shortcut-name-move-group-left = Move group left
shortcut-name-move-group-right = Move group right
shortcut-name-move-group-to-previous-strip = Move group to strip above
shortcut-name-move-group-to-next-strip = Move group to strip below
shortcut-name-move-group-to-previous-display = Move group to display on left
shortcut-name-move-group-to-next-display = Move group to display on right
shortcut-name-resize-narrower = Make window narrower
shortcut-name-resize-wider = Make window wider
shortcut-name-reopen-closed-tab = Reopen closed tab
shortcut-name-focus-group-1 = Window 1
shortcut-name-focus-group-2 = Window 2
shortcut-name-focus-group-3 = Window 3
shortcut-name-focus-group-4 = Window 4
shortcut-name-focus-group-5 = Window 5
shortcut-name-focus-group-6 = Window 6
shortcut-name-focus-group-7 = Window 7
shortcut-name-focus-group-8 = Window 8
shortcut-name-focus-group-9 = Window 9
shortcut-name-focus-strip-1 = Strip 1
shortcut-name-focus-strip-2 = Strip 2
shortcut-name-focus-strip-3 = Strip 3
shortcut-name-focus-strip-4 = Strip 4
shortcut-name-focus-strip-5 = Strip 5
shortcut-name-focus-strip-6 = Strip 6
shortcut-name-focus-strip-7 = Strip 7
shortcut-name-focus-strip-8 = Strip 8
shortcut-name-focus-strip-9 = Strip 9
shortcut-name-open-files = Open Files
shortcut-name-open-terminal = Open a terminal
shortcut-name-clipboard-history = Clipboard history
shortcut-name-notifications = Notifications
shortcut-name-toggle-fullscreen = Full screen
shortcut-name-toggle-magnifier = Magnifier
shortcut-name-magnifier-zoom-in = Magnifier zoom in
shortcut-name-magnifier-zoom-out = Magnifier zoom out
shortcut-name-power-menu = Power menu
shortcut-name-next-input-source = Next keyboard layout
shortcut-name-emoji-picker = Emoji
shortcut-name-say-something = Say something here
shortcut-name-ask-agent = Ask agent about selection

# The groups desktop shortcuts are listed in
shortcut-category-system = System
shortcut-category-navigation = Navigation
shortcut-category-windows = Windows
shortcut-category-apps = Apps
shortcut-category-capture = Capture
shortcut-category-accessibility = Accessibility

# The keyboard shortcuts resource on the action door
keyboard-shortcuts-resource = Keyboard shortcuts
keyboard-shortcuts-list = List keyboard shortcuts
keyboard-shortcuts-list-description = Every desktop shortcut with its label, current and default chords, whether it can be changed, and its category
keyboard-shortcuts-reset-all = Restore all default shortcuts
keyboard-shortcuts-reset-all-description = Put every changeable desktop shortcut back on its default chord
keyboard-shortcut-rebind = Change the shortcut for { $shortcut }
keyboard-shortcut-rebind-description = { $shortcut } is on { $chords } (default { $defaults }). Give it another chord; a chord something else uses is refused, naming what uses it.
keyboard-shortcut-reset = Restore the default shortcut for { $shortcut }
keyboard-shortcut-reset-description = Put { $shortcut } back on { $defaults }
keyboard-shortcut-conflict = { $chord } is already used for “{ $shortcut }”.
keyboard-shortcut-invalid = That shortcut is not supported.

# Shortcut guide (F1)
guide-title = Keyboard shortcuts
guide-reopen-hint = Select a navigation shortcut to change it
guide-done = Done
guide-press-new-shortcut = Press a new shortcut for “{ $action }” · Esc cancels
guide-message-esc-cancels = { $message } · Esc cancels
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
guide-controller-intro = Move with the stick or D-pad. Hold { $guide } to arrange windows. In a fullscreen game only { $guide } chords reach the desktop.
guide-row-move-focus = Move focus
guide-row-select = Select
guide-row-back = Back
guide-row-app-actions = Actions for this app
guide-row-search = Search
guide-row-this-guide = This guide
guide-row-windows = Windows
guide-row-tabs = Tabs
guide-row-move-tab = Move tab
guide-row-maximize = Maximize or restore
guide-row-strips = Strips
guide-row-workspaces = Workspaces
guide-row-move-group = Move group
guide-row-move-group-strip = Move group to strip
guide-row-move-window = Move window
guide-row-resize = Resize
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
guide-navigation-heading = Edit navigation shortcuts
guide-essentials-heading = Desktop essentials
guide-group-windows = Windows and workspaces
guide-group-search = Search and system
guide-group-capture = Capture and display
guide-overview = Overview
guide-unassigned = Unassigned
guide-unassigned-inline = unassigned
guide-pointer = Pointer: click a window to focus it, drag tabs or groups to reorder them, drag an edge to resize
guide-strip-jump = Go straight to a strip with { $first } … { $last }; strip 1 is the top one
guide-app-tab-chords = { $next }, { $previous } and { $reopen } go to the app first; the desktop’s tab shortcuts here always work
guide-your-shortcuts = Your shortcuts: { $shortcuts }
guide-vim-mode = Vim keys: h j k l
guide-a11y-name = Keyboard shortcuts
guide-a11y-description = Focus windows with { $window-back } or { $window-next }; tabs with { $tab-back } or { $tab-next }; maximize with { $maximize } and restore with { $restore }; strips with { $strip-back } or { $strip-next }, or straight to one with { $strip-first } through { $strip-last }; workspaces with { $workspace-back } or { $workspace-next }; move groups with { $group-left } or { $group-right }; resize with { $narrower } or { $wider }. Essential desktop actions: { $essentials }. Click a displayed navigation shortcut to change it; Escape cancels a pending change. A changed row gets a curved-arrow button that restores its defaults. With a mouse, click windows and tabs, drag tabs or empty tab-bar space to reorder, and drag window gaps to resize. Escape closes this guide.

# Devices and media, the Quick Settings drill-in

# Quick Settings
qs-title = Quick Settings
qs-a11y-description = Frequently used system controls
qs-on = On
qs-off = Off
qs-connected = Connected
qs-connecting = Connecting…
qs-not-connected = Not connected
qs-tile-settings = Settings
qs-tile-settings-description = Open device settings
qs-tile-power = Power
qs-tile-power-description = Lock, log out, restart or shut down
qs-volume = Volume
qs-brightness = Brightness
qs-brightness-display = Brightness, { $display }
qs-slider-a11y-description = Use Left and Right Arrow to adjust
qs-network = Network
qs-wired-network = Wired network
qs-cable-unplugged = Cable unplugged
qs-bluetooth = Bluetooth
qs-vpn = VPN
qs-night-light = Night Light
qs-airplane-mode = Airplane mode
qs-mobile-data = Mobile data
qs-do-not-disturb = Do Not Disturb
qs-do-not-disturb-on = Notifications held
qs-rotation-lock = Rotation lock
qs-rotation-locked = Locked
qs-rotation-unlocked = Rotates with device
qs-devices = Devices
qs-devices-audio-and-media = Audio and media
qs-devices-count = { $count ->
    [one] 1 device
   *[other] { $count } devices
}
qs-devices-a11y-description = Personal devices, audio outputs and media routes
qs-clipboard = Clipboard
qs-clipboard-empty = Empty
qs-clipboard-count = { $count ->
    [one] 1 item
   *[other] { $count } items
}
qs-clipboard-a11y-description = Private clipboard history
qs-power-saver = Power Saver
qs-power-balanced = Balanced
qs-power-performance = Performance
qs-power-ac = AC power

# Quick Settings: editing the control grid
qs-edit-controls = Edit controls
qs-edit-help = Drag or press Ctrl + Up/Down to reorder
qs-edit-a11y-description = Edit controls; drag or use Control plus Arrow keys to reorder
qs-edit-active-heading = Active controls
qs-edit-available-heading = Available
qs-edit-reset = Reset defaults
qs-edit-reset-a11y = Reset Quick Settings defaults
qs-edit-done = Done
qs-edit-done-a11y = Finish editing Quick Settings
qs-edit-required-a11y = Required Quick Settings control
qs-edit-remove-a11y = Remove Quick Settings control
qs-edit-add-a11y = Add Quick Settings control
qs-edit-required-control-a11y = Required; use Control plus Arrow keys to reorder
qs-edit-optional-control-a11y = Adds or removes this control. Control plus Arrow keys reorder it.

# Quick Settings: Wi-Fi
qs-wifi = Wi-Fi
qs-wifi-off = Wi-Fi is off
qs-wifi-scanning = Scanning for networks…
qs-wifi-scan-failed = Couldn't scan for networks
qs-wifi-none = No networks found
qs-wifi-secured = Secured
qs-wifi-open = Open
qs-wifi-turn-on = Turn on Wi-Fi
qs-turning-on = Turning on…
qs-turning-off = Turning off…
qs-wifi-problem-hardware-switch = Off with the hardware switch
qs-wifi-problem-stayed-off = Couldn't turn on
qs-wifi-problem-failed = Couldn't change Wi-Fi
qs-power-mode = Power mode
qs-power = Power mode
qs-lock-description = Lock the screen; apps keep running
qs-notifications = Notifications
qs-notifications-none = None
qs-notifications-count = { $count }
qs-battery = Battery
qs-battery-charging = Charging
qs-battery-full = Fully charged
qs-battery-not-charging = Plugged in, not charging
qs-battery-on-battery = On battery
qs-battery-time-left = { $hours ->
    [0] { $minutes } min left
   *[other] { $hours } h { $minutes } min left
}
qs-battery-time-to-full = { $hours ->
    [0] Full in { $minutes } min
   *[other] Full in { $hours } h { $minutes } min
}
qs-battery-settings = Battery settings
qs-power-settings = Power settings
qs-awake-app = { $app } keeps the screen on
qs-awake-apps = { $count } apps keep the screen on
qs-wifi-settings = Wi-Fi settings
qs-wifi-known = Known
qs-wifi-password-for = Enter the network's password to join
qs-bluetooth-off = Bluetooth is off
qs-bluetooth-turn-on = Turn on Bluetooth
qs-bluetooth-pair = Pair a new device
qs-bluetooth-paired = Paired
qs-bluetooth-use-elsewhere = Use on another device
qs-vpn-settings = VPN settings
qs-vpn-details = VPN profiles
qs-sound-output = Sound output
qs-sound-settings = Sound settings
qs-devices-settings = Devices & sync settings
qs-clipboard-clear-history = Clear history
qs-clipboard-copied = { $age }, from { $source }
qs-hosted-row-a11y = { $title }. { $detail }
qs-hosted-row-action-a11y = { $title }. { $detail }. { $action }

# Quick Settings: Bluetooth
qs-bluetooth-empty = No devices found
qs-bluetooth-device-connected-a11y = Connected; open device actions
qs-bluetooth-device-paired-a11y = Paired; activate to connect

# Quick Settings: VPN
qs-vpn-empty = No VPN profiles. Add one in Settings.

# Quick Settings failures, as notifications
qs-action-failed-refreshing = { $error }. The current device state is being refreshed.
qs-bluetooth-connect-failed = Could not connect { $device }
qs-bluetooth-disconnect-failed = Could not disconnect { $device }
qs-vpn-change-failed = Could not change { $profile }

# Portal and permission sheets: app choice, global shortcuts, reviews, administrator access, network secrets, app-action grants
portal-choose-app-title = Choose an application
portal-chooser-this-item = this item
portal-chooser-detail = { $app } wants to open { $subject }
portal-open = Open
portal-cancel = Cancel
portal-allow = Allow
portal-authenticate = Authenticate
portal-connect = Connect
portal-shortcuts-title = Set global shortcuts
portal-shortcuts-detail = Select a row, then press the shortcut. Tilo and active app shortcuts cannot be claimed. Backspace clears a row.
portal-save-shortcuts = Save shortcuts
portal-shortcut-not-set = Not set
portal-shortcut-needs-modifier = Hold Ctrl, Alt, Shift, or Super, then press a key.
portal-shortcut-reserved = Reserved by Tilo. Choose another shortcut.
portal-shortcut-duplicate = Already assigned in this request. Choose another shortcut.
portal-shortcut-taken = Already used by { $app }. Choose another shortcut.
portal-shortcut-another-app = another application
portal-auth-heading = Administrator access
portal-auth-ended = Authentication ended unexpectedly.
portal-auth-rejected = That password was not accepted.
portal-grant-title = { $app } wants to use { $target }
portal-grant-detail-confirms = { $risk }
    It still asks you to confirm each time it runs.
portal-grant-detail-unattended = { $risk }
    It runs without asking you again.
portal-grant-once = Allow once
portal-grant-session = Allow for this session
portal-grant-always = Always allow in this workspace
portal-grant-deny = Deny
portal-confirm-title = { $who } wants to act for you
portal-confirm-detail = { $risk }. Only this once; nothing is remembered.
portal-moderation-report = Send this report once with your signing identity
portal-moderation-appeal = Send this appeal once with your signing identity
portal-moderation-destination = Destination
portal-moderation-owner = Destination owner key
portal-moderation-identity = Your signing identity (root key)
portal-moderation-target = Package or case
portal-moderation-category = Category
portal-moderation-content = Content to send
portal-moderation-evidence = Evidence digest
portal-risk-none = Only shows information
portal-risk-low = Low risk: an ordinary, reversible change
portal-risk-destructive = Destructive: can delete or overwrite
portal-risk-privileged = Privileged: crosses a security boundary
portal-risk-sensitive = Sensitive: reads or exports private data
portal-usb-title = Choose USB devices
portal-usb-reason = Select devices individually; read-write can modify a device.
portal-usb-resource = Space or click toggles a row. Access ends on release or disconnect.
portal-usb-confirm = Allow selected
portal-usb-unnamed-device = USB device
portal-usb-row-read-write = Read and write
portal-usb-row-read-only = Read only
portal-account-title = Share your identity?
portal-account-reason = The app is asking for your name and account identifier.
portal-account-resource = Your selected identity for this workspace
portal-account-confirm = Share identity
portal-launcher-web-app-title = Install this web app?
portal-launcher-title = Add this launcher?
portal-launcher-reason = Review the launcher before it is added to your application library. The supplied image is checked but never opened from an app path or file descriptor.
portal-launcher-confirm = Add launcher
portal-launcher-icon-supplied = Supplied image (validated)
portal-launcher-icon-monogram = Tilo monogram
portal-chooser-nothing-granted = This app has been given no files to choose from.
portal-chooser-file-not-granted = This app has not been given { $path }.
portal-chooser-no-file = There is no file to open.
portal-chooser-session-closing = The session is closing.
portal-chooser-other-workspace = The app's workspace is not the one on screen.
portal-chooser-open-uri-undeclared = This app has not declared that it opens links.
portal-launcher-icon-compass = Tilo web compass
portal-background-title = Allow background activity?
portal-background-resource = The app may keep working after its window is closed.
portal-background-confirm = Allow in background
portal-wallpaper-title = Set workspace wallpaper?
portal-wallpaper-preview = A preview will be shown before this private workspace setting is applied.
portal-wallpaper-private = This changes only this device's private workspace backdrop.
portal-wallpaper-resource = Apply to { $target }
    Private imported image
portal-wallpaper-confirm = Set wallpaper
portal-email-title = Open your mail app?
portal-email-reason = Review the prefilled message before it is handed to your configured mail provider.
portal-email-confirm = Open mail app
portal-email-cc = Cc: { $address }
portal-email-bcc = Bcc: { $address }
portal-email-no-recipient = No recipient prefilled
portal-email-subject = Subject: { $subject }
portal-email-attachments = { $count ->
    [one] 1 attachment will be available through the document portal.
   *[other] { $count } attachments will be available through the document portal.
}
portal-remember-device = Remember on this device
portal-a11y-name = Name
portal-a11y-answer = Answer
portal-secret-required = Fill in every required field.
portal-secret-wifi-length = A Wi-Fi password is 8 to 63 characters, or 64 hexadecimal digits.
portal-network-heading = Network
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

# Top bar
topbar-clocks-notification-title = Clocks
topbar-clocks-notification-body = Time is up

# Workspace activity: what a collaborator is doing right now
host-activity-working-on = Working on { $target }
host-activity-working-in = Working in { $app }
host-activity-editing = Editing shared content
host-activity-organizing = Organizing shared content
host-activity-updating-details = Updating workspace details
host-activity-reviewing-discussion = Reviewing the discussion
host-activity-cleaning-up = Cleaning up shared content
host-activity-restoring = Restoring shared content
host-activity-managing-access = Managing workspace access
host-activity-maintaining-history = Maintaining workspace history
host-activity-reviewing-sensitive = Reviewing sensitive history

# Screen sharing, as the People popover describes it
host-exposure-kind-unidentified = unidentified session
host-exposure-kind-agent = agent
host-exposure-kind-person = person
host-exposure-surface = a window
host-exposure-window-in-workspace = { $window } in { $workspace }
host-exposure-controls = { $name } ({ $kind }) controls { $target }
host-exposure-views = { $name } ({ $kind }) views { $target }

# Invitations
host-invite-unavailable-title = Invite unavailable
host-invite-unavailable-body = You need permission to invite people to this workspace.
host-this-workspace = this workspace

# Notification sources
host-source-applications = Applications
host-source-system = System
host-source-app-actions = Application actions

# App launches and session actions
host-app-did-not-open-title = Application did not open
host-app-did-not-open-body = The app started but did not create a window. Check the session log and try again.
host-session-action-failed = Session action failed
host-session-lock-failed = Couldn’t lock the session. Check the session log and try again.
host-session-logout-failed = Couldn’t log out. Check the session log and try again.
host-session-switch-user-failed = Couldn’t switch users. Check the session log and try again.
host-session-suspend-failed = Couldn’t suspend. Check the session log and try again.
host-session-restart-failed = Couldn’t restart. Check the session log and try again.
host-session-shut-down-failed = Couldn’t shut down. Check the session log and try again.
host-session-action-blocked = Session action blocked

# Search and app actions
host-action-started = Started “{ $title }”
host-action-cannot-run = Can't run “{ $title }”
host-action-failed-status = Couldn't run
host-action-check-values = Check the values and try again.
host-action-try-again = Something went wrong. Try again.
uri-open-invalid = This link can't be opened.
uri-open-no-settings = Settings isn't installed on this device.
uri-open-no-handler = No installed app opens this kind of link.
uri-open-no-workspace = Open a workspace, then try again.
launchpad-action-unanswered = The app never answered
host-search-placeholder = Search…
host-search-in = Search in { $scope }…
host-search-in-resource-fallback = resource
host-search-scope-app = { $title } app
host-a11y-filter-all = Filter: All
host-a11y-filter-more = Filter: More
host-a11y-filter-kind = Filter: { $kind }
host-a11y-scope = Scope: { $scope }
host-a11y-search-filters = Search filters
host-a11y-result-kinds = Result kinds
host-a11y-search-scope = Search scope

# Tabs
host-tab-duplicate-failed = Can't duplicate this tab
host-tab-move-failed = Can't move this tab
host-a11y-tab-menu = { $title } tab menu

# Wi-Fi joins from Quick Settings
host-wifi-connected-title = Connected to { $ssid }
host-wifi-connected-shared-credential = Used the password saved on one of your devices.
host-wifi-connected-sharing-unavailable = Connected, but encrypted sharing is temporarily unavailable.
host-wifi-connected-shared = Connected. Your other devices can join with the same password.
host-wifi-connected-saved = Connected. This saved network will rejoin automatically.
host-wifi-connected-open = Connected to the open network.
host-wifi-saved-password-stale = The saved password no longer works. Enter the current password.
host-wifi-join-check-password = Couldn't join. Check the password.
host-wifi-connect-failed-title = Couldn’t connect to { $ssid }
host-wifi-connect-failed-body = The network did not accept the connection. Try again from Wi‑Fi settings.

# Quick Settings controls the device refused
host-qs-change-failed-brightness = Couldn’t change brightness
host-qs-change-failed-volume = Couldn’t change volume
host-qs-change-failed-value-restored = The control was restored to the current device value.
host-qs-change-failed-wifi = Couldn’t change Wi-Fi
host-qs-change-failed-bluetooth = Couldn’t change Bluetooth
host-qs-change-failed-wired = Couldn’t change wired network
host-qs-change-failed-airplane = Couldn’t change airplane mode
host-qs-change-failed-hotspot = Couldn’t change personal hotspot
host-qs-change-failed-mobile = Couldn’t change mobile broadband
host-qs-change-failed-power = Couldn’t change power mode
host-qs-change-failed-state-restored = The control was restored to the current device state.

# Workspace history outcomes
host-history-partial-undo-title = Partially undid the selected work
host-history-partial-undo-body = Later collaborator edits were preserved. History shows which parts were left intact.

# Call handoff between devices
host-call-handoff-title = Call handoff
host-call-handoff-waiting = Waiting for explicit approval on { $device }; this call stays live here
host-call-move-failed = Could not move call

# Devices and media: what an action in the drill-in did
host-fabric-notice-title = Devices & media
host-fabric-action-failed = Could not complete device action
host-fabric-speaker-playing-media = Playing on { $group }; the media keeps playing from here
host-fabric-asked-capture = Asked { $device } to share its { $input } once
host-fabric-asked-audio = Asked { $device } to allow this audio once; playback stays local until approved
host-fabric-asked-tethering = Asked { $device } to share its connection
host-fabric-asked-controller = Asked { $device } to allow this controller once for the focused app
host-fabric-media-preparing = Preparing media for { $device }; this device stays active until it opens
host-fabric-bluetooth-moving = Moving { $accessory } to { $device }; it stays here until that device confirms its pairing
host-fabric-place-left = { $device } will sit left of this display after it confirms
host-fabric-place-right = { $device } will sit right of this display after it confirms
host-fabric-place-above = { $device } will sit above this display after it confirms
host-fabric-place-below = { $device } will sit below this display after it confirms
host-fabric-control-granted = { $device } may control this device for 15 minutes

# Accessibility tree: workspace overlays
a11y-workspace = Tilo workspace
a11y-member-sheet = Workspace permission decision
a11y-member-sheet-description = Choose a clearly labelled permission option with Up or Down, press Enter to confirm, or Escape to cancel. Destructive changes require a separate explicit confirmation and offer workspace activity for review.
a11y-overview = Workspace overview
a11y-new-workspace = New workspace
a11y-workspace-settings = Workspace settings
a11y-workspace-settings-description = Workspace identity, people, permissions, apps, files, and signed ownership governance.
a11y-manage-ownership = Manage workspace ownership
a11y-manage-ownership-description = Open the same signed ownership actions available to search and automation. Recipient acceptance immediately completes a pending transfer.
a11y-workspace-name = Workspace name
a11y-workspace-settings-content = Workspace settings content
a11y-at-a-glance = At a glance

# Accessibility tree: clipboard history
a11y-clipboard-data = Clipboard data
a11y-clipboard-unpin = Unpin item
a11y-clipboard-pin = Pin item

# Accessibility tree: moving a Bluetooth accessory
a11y-bluetooth-accessory = Bluetooth accessory
a11y-bluetooth-disconnect = Disconnect { $accessory } from this device
a11y-bluetooth-move-to = Move to { $device }
a11y-bluetooth-destination-local = This device
a11y-bluetooth-destination-online = The destination confirms its own local pairing before this device disconnects
a11y-bluetooth-destination-offline = Offline device
a11y-bluetooth-move = Move { $accessory }
a11y-bluetooth-move-description = Choose one of your devices already paired with this accessory. Pairing keys never leave either device. Use Up and Down, Page Up and Page Down, Home, or End to reach every destination.
a11y-bluetooth-destinations = Bluetooth destinations

# Accessibility tree: devices and media rows
a11y-certified-display = Your other display
a11y-placement-apply = Apply this reversible display placement
a11y-placement-dismiss = Dismiss this display placement suggestion
a11y-placement-undo = Undo placement for { $display }
a11y-display-fallback = display
a11y-speaker-join = Join { $group } as the { $role }
a11y-speaker-join-description = { $leader } asked; you are asked once and can leave at any time
a11y-speaker-join-allow = Join this speaker group
a11y-speaker-join-decline = Decline this speaker group
a11y-speaker-playing-on = Playing on { $group }
a11y-speaker-setup = Set up speakers
a11y-speaker-membership = { $role } in { $group }
a11y-speaker-membership-description = This device plays in this speaker group; activate to leave
a11y-remote-audio-play-here = Play { $media } here
a11y-remote-audio-allow = Allow this remote audio once
a11y-remote-audio-decline = Decline this remote audio
a11y-remote-capture-allow = Allow this remote input once
a11y-remote-capture-decline = Decline this remote input
a11y-remote-capture-session = Remote { $input }
a11y-remote-capture-provisioning = Preparing a private local endpoint
a11y-remote-capture-unavailable = Remote { $input } unavailable
a11y-call-continue = Continue call in { $room }
a11y-call-allow = Allow this call handoff once
a11y-call-decline = Decline this call handoff
a11y-call-in = Call in { $room }
a11y-bluetooth-pair = Pair { $accessory }
a11y-bluetooth-pair-allow = Allow pairing for this handoff once
a11y-bluetooth-pair-decline = Decline this Bluetooth handoff
a11y-controller-use = Use { $controller }
a11y-controller-allow = Allow this controller once
a11y-controller-decline = Decline this controller
a11y-media-handoff = { $media } on { $target }
a11y-input-lower = Lower input level
a11y-input-raise = Raise input level
a11y-tethering-allow = Allow tethering once
a11y-tethering-decline = Decline tethering
a11y-control-revoke = Revoke control of this device
a11y-control-allow = Allow this device to control this device for 15 minutes

# Inline permission prompt: what an app asks for
capability-filesystem-read = read access to { $path }
capability-filesystem-read-write = read/write access to { $path }
capability-camera = Camera access
capability-microphone = Microphone access
capability-screencast = Screen capture
capability-remote-desktop = Remote control (pointer + keyboard)
capability-print = Send documents to a printer
capability-host-shell = Open this device's own shell, outside every sandbox
capability-system-info = See how busy this device is
capability-processes = See the programs running on this device and end your own
capability-process-commands = See the command lines of running programs
capability-io-uring = Use the kernel's fast asynchronous input and output (io_uring)
capability-network-hosts = Network access to { $hosts }
capability-network = Network access

# The session's agent door: a client in this login (an AI agent in a
# terminal) asks to work in a workspace as an agent member.
agent-door-client = { $client } ({ $program })
agent-door-client-in-app = { $who } in { $app }
agent-door-admission = Work in “{ $workspace }” as the agent { $agent }
agent-door-param-agent = The agent to admit it as: an existing agent member, or a new one by this name. Defaults to the name the prompt proposes.
agent-door-param-preset = A new agent's preset: Observer, Contributor, Operator, or Custom. Presets never grant access to a surface by themselves.
agent-door-preset-invalid = Choose Observer, Contributor, Operator, or Custom.
agent-door-agent-invalid = An agent needs a name of up to 128 characters.
agent-door-workspace-gone = The workspace this client asked to work in is gone.
agent-door-not-administrator = You do not administer this workspace, so { $agent } was not added.
agent-door-no-workspace = There is no workspace here to work in.
agent-door-withdrawn = The request was withdrawn before anyone answered it.
agent-door-declined = The person at this desktop declined this connection.

# The shortcut-guide pill in the display corner
help-pill = Help

# Workspace overview panel
overview-go-to = Go to
overview-no-room = No room selected
overview-you-are-here = You are here
overview-shared-out = Shared out of here
overview-more-shares = and { $count } more
overview-share-with-two = { $first } and { $second }
overview-share-with-many = { $count ->
        [one] { $first } and { $count } other
       *[other] { $first } and { $count } others
    }
overview-share-can-edit = { $with } can edit
overview-new-workspace = New workspace
overview-join-workspace = Join a workspace…
overview-leave-workspace = Leave
# A site's workspace in the switcher: the community this computer follows, or
# the site it came from.
overview-site-community = Community · { $site }
overview-site-web = On the web
overview-site-here = { $count ->
    [one] 1 person here
   *[other] { $count } people here
}
a11y-leave-workspace = Leave { $name }

# Following someone's view
follow-label = Following { $person }
follow-stopped = Stopped following
follow-stop = Stop

# Search filter chips
search-filter-all = All
search-filter-more = More

# Quick Settings: devices and media rows
qs-devices-mic-other = Not in use
qs-devices-mic-current = Current microphone
qs-devices-mic-level = { " " }· input level { $level }%
qs-devices-mic-muted = { " " }· muted
qs-devices-this-device = This device
qs-devices-offline = Offline
qs-devices-sending = Sending, { $percent }%
qs-devices-sent = Sent
qs-devices-transfer-failed = Couldn't send. The original is still here.
qs-devices-mutual-control = You control each other
qs-devices-may-control = Can control this device
qs-devices-control-granted = You can control it. Place it beside this display.
qs-devices-play-here = Play { $media } here
qs-devices-audio-only = Audio only for this video
qs-devices-no-output = No media output
qs-devices-unavailable-media = Can't play this
qs-devices-online = Online
qs-devices-health = { $power }
qs-devices-external-power = external power
qs-devices-charging = charging { $percent }%
qs-devices-battery = battery { $percent }%

# Quick Settings: moving a Bluetooth accessory
qs-bluetooth-move-note = The other device must already be paired with it
qs-devices-fabric-unavailable = Personal device fabric is unavailable
qs-bluetooth-this-device = This device
qs-bluetooth-disconnect = Disconnect { $accessory } without forgetting it
qs-bluetooth-this-certified = This device
qs-bluetooth-move-here = Move here after its local pairing is confirmed
qs-devices-showing = { $first }–{ $last } of { $total }

# Quick Settings: devices and media pane
qs-devices-no-identity = Set up your personal identity to connect devices
qs-devices-empty = No outputs or devices
qs-devices-place = Place { $display } { $side ->
        [left] left
        [right] right
        [above] above
       *[below] below
    }
qs-devices-place-note = Suggested by this device's sensors. Check it before applying.
qs-devices-placement-undo-note = Restore the exact placement from before calibration
qs-devices-speaker-join = Join { $group } as the { $role }?
qs-devices-speaker-join-note = { $leader } asked. You can leave any time.
qs-devices-speaker-join-note-output = { $leader } asked to play on { $output }. You can leave any time.
qs-devices-speaker-for = Playing for { $peer }
qs-devices-another-device = another device
qs-devices-speaker-connecting = { $playing } playing, { $waiting } connecting
qs-devices-speakers-playing = { $count ->
        [one] { $count } speaker playing
       *[other] { $count } speakers playing
    }
qs-devices-speaker-syncing = { $playing } playing, { $syncing } syncing…
qs-devices-speaker-clock-failed = { $count ->
        [one] { $count } speaker can't keep time: { $reason }
       *[other] { $count } speakers can't keep time: { $reason }
    }
qs-devices-speaker-setup-note = Stereo pair or room speakers, in Settings
qs-devices-speaker-membership-note = Set up on { $leader }
qs-devices-speaker-listen = Let { $leader } use this microphone for { $group }?
qs-devices-speaker-listen-note = Only while calibrating. The recording stays on this device.
qs-devices-speaker-listen-note-microphone = Uses { $microphone } only while calibrating. The recording stays on this device.
qs-devices-speaker-listener-note = Set up on { $leader }
qs-devices-another-certified = Another of your devices
qs-devices-requested-allow-once = { $source } asked for this
qs-devices-audio-app = Audio of the focused app
qs-devices-audio-system = All system audio. Choose a device below.
qs-devices-certified-device = Another device
qs-devices-audio-awaiting = Waiting for { $peer } to allow it
qs-devices-audio-streaming = Streaming to { $peer }
qs-devices-audio-interrupted = Interrupted on { $peer }
qs-devices-audio-syncing = Syncing with { $peer }…
qs-devices-audio-clock-failed = Can't keep time with { $peer }: { $reason }
qs-devices-capture-share = Share this device's { $input }
qs-devices-capture-requested = { $source } asked for this for { $app }
qs-devices-capture-use-remote = Use remote { $input }
qs-devices-capture-source-note = Choose a device below. It asks for approval there.
qs-devices-capture-waiting-on = Waiting for approval on { $peer }
qs-devices-capture-allow-app = Allow the focused app once
qs-devices-capture-waiting-ready = Waiting for { $peer } to become ready
qs-devices-capture-private = Private session with { $peer }
qs-devices-capture-source-unavailable = Source cannot provide this input
qs-devices-capture-receiver-unavailable = Private receiver could not be created
qs-devices-capture-interrupted = Connection lost. The session ended.
qs-devices-call-requested = { $source } asked for this
qs-devices-call-source-note = Live here. Choose a device below.
qs-devices-bluetooth-requested = { $source } asked for this handoff
qs-devices-handoff-checking = Checking { $peer }…
qs-devices-handoff-awaiting = Waiting for approval on { $peer }
qs-devices-handoff-releasing = Releasing here…
qs-devices-handoff-pairing = Pairing on { $peer }…
qs-devices-handoff-connecting = Connecting on { $peer }…
qs-devices-handoff-confirming = Connected, confirming…
qs-devices-handoff-connected = Now connected to { $peer }
qs-devices-handoff-failed-retry = { $reason }
qs-devices-cancelled-retry = Cancelled
qs-devices-cancelled = Cancelled
qs-devices-controller-requested = { $source } asked for this for { $app }
qs-devices-controller-local = This controller. Choose a device below.
qs-devices-controller-forwarding = Forwarding to { $target }
qs-devices-controller-awaiting = Waiting for approval on { $target }
qs-devices-media-waiting = Waiting for the other device
qs-devices-media-opened = Opened on destination
qs-devices-media-failed = Couldn't move it: { $reason }
qs-devices-media-playing = Playing
qs-devices-media-paused = Paused
qs-devices-output-default = In use
qs-devices-output-route = Not in use
qs-devices-output-bluetooth = Bluetooth { $codec }
qs-devices-tether-requesting = Waiting for phone approval
qs-devices-tether-consent = Allow once or decline
qs-devices-tether-preparing = Starting this phone's hotspot
qs-devices-tether-connecting = Joining the phone's hotspot
qs-devices-tether-requester = Connected through this phone
qs-devices-tether-provider = Sharing this phone's connection
qs-devices-action-stop = Stop
qs-devices-action-cancel = Cancel
qs-devices-action-retry = Try again
qs-devices-action-dismiss = Dismiss
qs-devices-action-leave = Leave
qs-devices-action-disconnect = Disconnect
qs-devices-tether-denied = Declined on the phone
qs-devices-tether-expired = Request expired
qs-devices-tether-offline = The device went offline
qs-devices-tether-disabled = Wi-Fi Continuity was disabled
qs-devices-tether-failed = Could not establish tethering
qs-devices-tether-disconnected = Disconnected
qs-devices-tether-with = Tethering with { $peer }
qs-devices-ask-phone = Ask it to share its connection
qs-devices-use-controller = Use the controller here
qs-devices-use-input = Use this device's { $input }
qs-devices-play-audio-here = Play the audio here
qs-devices-no-audio = This device has no audio output
qs-devices-place-this-left = Place this display to the left
qs-devices-place-this-right = Place this display to the right
qs-devices-place-this-above = Place this display above
qs-devices-place-this-below = Place this display below

# Quick Settings: clipboard history
qs-clipboard-search-matches = { $count ->
        [one] One copy matches “{ $query }”
       *[other] { $count } copies match “{ $query }”
    }
qs-clipboard-empty-hint = Nothing copied yet
qs-clipboard-no-matches = Nothing matches “{ $query }”
qs-clipboard-search-range = { $first } to { $last } of { $total } copies. Type to search.
qs-clipboard-search-count = { $count ->
        [one] One copy. Type to search.
       *[other] { $count } copies. Type to search.
    }
qs-clipboard-clear-a11y = Clear unpinned clipboard history
qs-clipboard-mime-bytes = { $mime } · { $bytes } bytes
qs-clipboard-remove-a11y = Remove clipboard item

# Quick Settings: joining a protected Wi-Fi network
qs-wifi-protected = This network is protected

# Retained history: sources and severities
history-this-device = This device

# On-screen display for the hardware keys
osd-play = Play
osd-pause = Pause
osd-next-track = Next track
osd-previous-track = Previous track
osd-stop = Stop
osd-percent = { $percent }%
osd-off = Off
osd-muted = Muted
osd-volume = Volume { $percent }%
osd-brightness = Brightness { $percent }%
osd-brightness-display = Brightness { $percent }%, { $display }
osd-display-percent = { $display } { $percent }%
osd-all-displays = All displays
osd-microphone-muted = Microphone muted
osd-microphone-on = Microphone on
osd-keyboard-backlight-off = Keyboard backlight off
osd-keyboard-backlight = Keyboard backlight { $percent }%
osd-no-backlight = No adjustable display
osd-no-agent = No agent is available here
osd-starting-agent = Starting the agent…
osd-airplane-mode-on = Airplane mode on
osd-airplane-mode-off = Airplane mode off
osd-touchpad-on = Touchpad on
osd-touchpad-off = Touchpad off
osd-magnifier-off = Magnifier off
osd-magnifier = Magnifier { $zoom }
osd-pointer-held = The app holds the pointer. { $chord } releases it.
osd-pointer-released = Pointer released
osd-mouse-keys-on = Mouse keys on
osd-mouse-keys-off = Mouse keys off

# System status, the tray's accessible summary
status-wired = Wired
status-wired-named = Wired: { $name }
status-wifi = Wi-Fi
status-wifi-named = Wi-Fi: { $name }
status-offline = Offline
status-muted = Muted
status-volume = Volume { $percent }%
status-battery-charging = Battery { $percent }%, charging
status-battery = Battery { $percent }%
status-summary-empty = System status
status-other-displays-a11y = Other displays

# Notification toasts
notification-source-caption = { $app } · From { $device }
notification-app-host-apps = Host Apps
notification-app-power = Power
notification-remote-input-stopped = Remote input stopped
notification-remote-input-stopped-sessions = Everyone connected to this computer was disconnected.
notification-remote-input-stopped-shares = { $count ->
    [one] Remote control was removed and sharing was stopped.
   *[other] Remote control was removed from { $count } shares; sharing was stopped.
}

# Media formats notices
codec-hardware-ready = Hardware H.264/HEVC is ready
codec-installed = More video and audio formats are on
codec-installed-mesa-waiting = { $applies } Hardware H.264/HEVC follows once a matching driver is published.
codec-failed = More video and audio formats could not be turned on
codec-failed-retry = Open Settings, Updates, to try again.
codec-failed-with-error = { $error } Open Settings, Updates, to try again.
codec-app-name = Video and audio formats
system-update-app-name = Updates
system-update-restart-title = Restart to finish updating
system-update-restart-body = Your windows come back after the restart.
network-cannot-join-app-name = Network
network-cannot-join-title = Couldn't join { $network }
network-cannot-join-body = Enter the password to connect.
network-cannot-join-action = Enter password
held-layers-app-name = Development
held-layers-title = { $count ->
    [one] A deployed layer is held on this system
   *[other] { $count } deployed layers are held on this system
}
held-layers-body = They are kept but not running, because they do not fit this system image. A new deploy or tilo-vm rollback replaces them.
    { $layers }
collaborator-needs-update = { $name } needs to update to keep working together
collaborator-needs-update-body = Your changes reach { $name } once they have.
collaborator-host-needs-update = This workspace’s host needs to update to keep working together
collaborator-host-needs-update-body = Your changes reach it once it has.
collaborator-update-self = Updating Tilo to keep working with { $name }
collaborator-update-self-host = Updating Tilo to keep working together
collaborator-update-self-body = Your changes stay here until then.

# Screen capture surfaces
capture-live-alt = Live capture: { $title }
capture-pinned-alt = Pinned screenshot: { $title }
capture-pinned-preview-alt = Pinned screenshot preview: { $title }
capture-live-selection = Live selection
capture-live-app = Live app
capture-live-screen = Live screen
capture-live-areas = { $count ->
    [one] { $count } live area
   *[other] { $count } live areas
}
capture-pinned-title = Screenshot ({ $width } × { $height })

# What a drag is carrying
drag-image = Image
drag-video = Video
drag-audio = Audio
drag-pdf = PDF document
drag-text = Text
drag-items = { $count ->
    [one] 1 item
   *[other] { $count } items
}

# App launch failures, as notifications
launch-error-provider-unavailable = The required app execution service is unavailable. Try again when it is available.
launch-error-not-installed = This app is no longer installed. Refresh the app list and try again.
launch-error-not-gui-app = This app does not provide a window.
launch-error-not-managed-service = This service cannot be opened as an app.
launch-error-not-authorized = You do not have permission to start this workspace service.
launch-error-launcher-unavailable = The app launcher is unavailable. Try again.
launch-error-no-display = The desktop is still starting. Try again in a moment.
launch-error-no-workspace = Choose a workspace, then try again.
launch-error-shell-owned-intent = Open this destination from the desktop controls.
launch-error-identity-unavailable = The workspace identity could not be loaded. Try again.
launch-error-registry-unreadable = The app list could not be read. Try again.
launch-error-sandbox-supervision = The app's file permissions could not be supervised. Try again.
launch-error-host-app-refused = This app's desktop entry cannot be started.
launch-failed-launcher-unavailable = The app launcher is unavailable. Open Applications and try again.
launch-failed-authority-changed = The app's file permissions changed while it was starting. Open it again.
launch-failed-authority-timeout = The app's file permissions were not ready in time. Open it again.
launch-failed-previous-instance = The previous copy of the app is still closing. Open it again in a moment.
launch-failed-stopped = The app stopped before opening. Open Applications and try again.
launch-failed-missing-library = The app needs a library this system doesn't have, so it stopped before opening.
launch-failed-runtime-mismatch = This app was built for a different Tilo runtime. Update or reinstall it to get the runtime it needs.
launch-failed-update = Needs an update to open. Try again when you’re online.
launch-failed-waiting-for-developer = Waiting for an update from the developer
launch-failed-system-behind = Opens once Tilo has updated
launch-failed-sandbox = The app's sandbox could not be set up, so it stopped before opening.
launch-failed-collaboration = The app couldn't join this workspace's collaboration service, so it stopped before opening. The desktop log says why.
launch-failed-window-lost = Its window stopped without being closed. Open it again from Applications; the desktop log says why.
launch-window-lost-title = { $app } closed unexpectedly
launch-failed-crashed = The app crashed before opening. Open Applications and try again.
launch-unnamed-app = application
launch-failure-title = Couldn't open { $app }

# An app, or a process it started, that the system ended because memory ran out
memory-app-closed-title = { $app } ran out of memory and was closed
memory-app-closed-body = Reopen it to continue where you left off.
memory-app-runaway-body = It kept taking more memory until the computer ran low. Reopen it to continue where you left off.
memory-job-stopped-title = { $process } in { $app } ran out of memory and was stopped
memory-job-stopped-body = { $app } is still open.
memory-job-runaway-body = It kept taking more memory until the computer ran low. { $app } is still open.
memory-app-reopen = Reopen

# Tab and window actions that could not be carried out
window-duplicate-no-tab = There is no tab to duplicate.
window-duplicate-foreign = Can't duplicate this window. Open another one from search.
window-tab-action-no-tab = There is no tab to act on.
window-tab-action-unknown-tab = That tab is no longer open.
window-tab-action-only-tab = This is the window's only tab; there is nothing to split it from.
window-tab-action-document-refused = Couldn't move the tab. Try again.

# Tab context menu, workspace toasts and placeholder tiles
shell-workspace-renamed = Workspace renamed
shell-tab-menu-close = Close tab
shell-tab-menu-split-right = Split right
shell-tab-menu-new-group = Move to new group
shell-terminal-unavailable = Terminal: { $error }
shell-app-unavailable = Application unavailable
shell-activity-title = Activity
shell-activity-loading = Loading workspace activity…

# Session (power) menu
power-menu-title = Power
power-menu-description-switch-user = Lock, log out, switch user, suspend, restart, or shut down this computer
power-menu-description = Lock, log out, suspend, restart, or shut down this computer
power-lock = Lock
power-lock-detail = Keep apps running
power-logout = Log out
power-logout-detail = Open apps close
power-switch-user = Switch user
power-switch-user-detail = Your apps keep running
power-suspend = Suspend
power-suspend-detail = Resume where you left off
power-restart = Restart
power-restart-detail = Open apps close
power-shut-down = Shut down
power-shut-down-detail = Open apps close

# Lock screen
lock-screen-a11y-name = Tilo lock screen
lock-screen-locked = Session locked
lock-screen-locked-at = Session locked, { $clock }
lock-screen-locked-at-date = Session locked, { $clock }, { $date }
lock-screen-password = Password
lock-screen-help = Type your password and press Enter
lock-screen-verifying-password = Verifying password
lock-screen-prompt-elsewhere = Password prompt is on the primary display
lock-screen-enter-password = Enter password
lock-screen-verifying = Verifying…
lock-screen-press-enter = Press Enter to unlock
lock-screen-cannot-verify = Can't verify your password. Enter it again.
lock-screen-sign-in-failed = Sign-in failed. Enter your password again.

# Bluetooth pairing prompt
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
bluetooth-prompt-pin-placeholder = PIN
bluetooth-prompt-code-placeholder = 6-digit code
bluetooth-prompt-reject = Don't pair
bluetooth-prompt-cancel = Cancel
bluetooth-prompt-accept-match = They match
bluetooth-prompt-accept-allow = Allow
bluetooth-prompt-accept-pair = Pair
bluetooth-prompt-code-label = Code shown on the device
bluetooth-prompt-pin-label = PIN for the device
bluetooth-prompt-a11y-code = Code { $code }

# Portal defaults: file picker buttons, notification default action, unnamed apps, capture sources
portal-file-save = Save
portal-file-choose-folder = Choose folder
portal-file-grant-write-folder = Allow this app to read and write the selected folder and its contents
portal-file-existing-write-folder = Choose a writable folder already accessible to this app
portal-file-allow-write-folder = Allow read and write
portal-file-use-write-folder = Use writable folder
portal-file-open = Open
portal-notification-open = Open
portal-inhibit-unnamed-app = An app
portal-capture-default-microphone = Default microphone
portal-capture-default-camera = Default camera
portal-capture-primary-display = Primary display
portal-capture-system-audio = System audio

# A speaker's role in a speaker group, as the $role of a sentence ("Join Living room as the right speaker")
speaker-role-left = left speaker
speaker-role-right = right speaker
speaker-role-centre = centre speaker
speaker-role-subwoofer = subwoofer
speaker-role-surround-left = left surround speaker
speaker-role-surround-right = right surround speaker
speaker-role-room = room speaker
speaker-group-summary = { $total ->
    [one] { $ready } of 1 speaker ready
   *[other] { $ready } of { $total } speakers ready
}
speaker-group-summary-waiting = { $total ->
    [one] { $ready } of 1 speaker ready, { $waiting } waiting for approval
   *[other] { $ready } of { $total } speakers ready, { $waiting } waiting for approval
}

# A remote input, as the $input of a sentence
remote-input-camera = camera
remote-input-microphone = microphone
remote-input-scanner = scanner


# Why moving media to another device failed, as the $reason of a sentence
media-handoff-failure-destination-unavailable = destination unavailable
media-handoff-failure-unsupported-output = destination cannot play this media
media-handoff-failure-invalid-workspace = workspace is unavailable there
media-handoff-failure-open-failed = destination could not open it
media-handoff-failure-timed-out = destination did not respond
media-handoff-failure-cancelled = cancelled

# Why moving a Bluetooth accessory failed, as the $reason of a sentence
bluetooth-handoff-failure-inspect-target = target inspection failed
bluetooth-handoff-failure-target-unavailable = target unavailable or declined
bluetooth-handoff-failure-disconnect-source = source could not release accessory
bluetooth-handoff-failure-pair-target = target pairing failed
bluetooth-handoff-failure-connect-target = target connection failed
bluetooth-handoff-failure-send = the connection was interrupted
bluetooth-handoff-failure-timed-out = transaction timed out
bluetooth-handoff-failure-rollback = source rollback failed

# Search result kinds, as the filter chips and group headers name them
search-kind-actions = Actions
search-kind-applications = Applications
search-kind-host-apps = Host Apps
search-kind-notes = Notes
search-kind-records = Records
search-kind-files = Files
search-kind-folders = Folders
search-kind-creators = Creators
search-kind-videos = Videos
search-kind-playlists = Playlists
search-kind-tabs = Tabs
search-kind-window-groups = Window groups
search-kind-answers = Answers
search-kind-people = People
search-group-recent = Recent

# Search scopes
search-scope-this-app = This app
search-scope-workspace = Workspace
search-scope-everywhere = Everywhere

palette-show-all = Show all { $count }
palette-value-on = On
palette-value-off = Off
palette-value-not-whole-number = Enter a whole number
palette-value-not-number = Enter a number
palette-value-not-on-off = Enter on or off
palette-value-not-understood = Enter a value this setting takes
palette-open-tab-here = As a tab here
palette-open-beside = Beside
palette-more-actions = More
palette-open-containing-folder = Open containing folder
palette-copy-path = Copy path

# Search providers, as the search settings actions name them
search-provider-now-playing = Now playing
search-provider-records = Records
search-provider-app-content = Content in your apps
# A locked app's entries (a password vault's logins) match what was typed:
# this row offers to unlock the app; Enter opens it to unlock.
search-unlock-app = Unlock { $app } to search for “{ $query }”
search-unlock-app-detail = { $app } is locked
search-provider-session = Session
search-provider-permission-prompts = Permission requests
permission-request-expired = Permission request is no longer pending
permission-request-owner-required = Permission decisions require the current owner
permission-request-permanent-owner-required = Permanent permission requires the current owner
permission-request-confirm = Confirm this permission decision
search-provider-shortcuts = Shortcuts
search-provider-tray = Tray
search-provider-files = Files
search-provider-quick-answers = Quick answers
search-provider-people = People
search-answer-sum = { $expression } =
search-answer-copied = Copied
search-answer-money-stale = { $amount } = · rates from { $age }
search-matches = Matches “{ $word }”
search-answer-open-address = Open { $address }
search-provider-web = Web
search-kind-web = Web
search-kind-websites = Websites
search-web-for = Search the web for { $query }
search-web-service-for = Search { $service } for { $query }
search-web-subtitle = Web
search-answer-no-browser = No app on this desktop opens web addresses.
search-answer-bad-address = That address can’t be opened.
search-people-invited = Invited
search-action-emoji-picker = Choose an emoji
search-action-emoji-picker-subtitle = Insert it where you’re typing
search-provider-host-apps = Host apps
search-provider-app-actions = Application resources and actions

# Search result badges

# Search: media transport for a running player
search-media-pause = Pause { $player }
search-media-play = Play { $player }
search-media-next = Next track in { $player }
search-media-previous = Previous track in { $player }
search-media-seek = Seek in { $player }

# Search: records
search-record-file-missing = The file this record names is no longer there.
search-record-in-this-workspace = In this workspace
search-record-in-workspace = In { $workspace }
search-record-in-another-workspace = In another workspace

# Search: files
search-files-no-app = No app on this desktop opens .{ $ext } documents.
search-files-folder-failed = Files couldn’t open this folder.
search-files-home = Home
search-files-location = { $folder } · { $age }

# Search: host apps

# Search: app action jobs and resources
search-app-job = Action job { $id }
search-app-job-cancel = Cancel action job { $id }
search-app-job-cancel-detail = Requests cooperative cancellation
search-app-job-accepted = accepted
search-app-job-running = running
search-app-job-reported-phase = Reported: { $phase }
search-app-job-reported-counts = Reported: { $phase } ({ $completed } / { $total })
search-app-job-reported-completed = Reported: { $phase } ({ $completed } completed)
search-app-job-reported-total = Reported: { $phase } (total { $total })
search-app-job-streaming = streaming
search-app-job-succeeded = succeeded
search-app-job-failed = failed
search-app-job-cancelled = cancelled
search-app-resource-contains = { $app } · contains resources
search-app-action-launches = { $resource } · { $app } · launches application

# Search: session actions
search-session-logout = Log out of Tilo
search-session-logout-detail = End this desktop session cleanly and return to the display manager
search-session-unsaved-work = An app has unsaved work
search-session-inhibited = { $reason }. Close the app or finish its work first.
search-session-not-offered = This session shares the computer with another desktop, which owns its power.

# Search: shortcuts
search-shortcut-run-on = Run on { $resource }

# Search: tray items
search-tray-open = Open { $item }
search-tray-in-top-bar = { $item } in the top bar
search-tray-entry = { $item }: { $entry }
search-tray-entry-on = { $item }: { $entry } (on)
search-tray-entry-off = { $item }: { $entry } (off)

# Search: desktop windows

# Search: shell actions, as palette rows
search-action-shortcut-help-toggle = Show keyboard shortcuts
search-action-shortcut-help-toggle-subtitle = F1 opens it anywhere; Super+/ is the configurable shortcut for the same guide
search-action-desktop-tour-open = Take the desktop tour
search-action-desktop-tour-open-subtitle = Revisit the three-minute introduction to navigation, organisation and collaboration
search-provider-device-local = Private windows
search-action-feedback-open = Give feedback
search-action-feedback-open-subtitle = Report a problem or an idea about the window in front; you review what is sent
feedback-action-unavailable = Feedback is not available on this desktop
search-action-overview-toggle = Show workspace overview
search-action-overview-toggle-subtitle = Show all workspaces and their windows
search-action-settings-open = Open workspace settings
search-action-settings-open-subtitle = Identity, people, privacy, apps and files for this workspace
search-action-settings-members-open = Open workspace people
search-action-settings-members-open-subtitle = People, roles and invitations in this workspace
search-action-settings-privacy-open = Open workspace privacy & security
search-action-settings-privacy-open-subtitle = App grants and enforced access for this workspace
search-action-settings-apps-open = Open workspace apps & files
search-action-settings-apps-open-subtitle = Shared app defaults and your workspace overrides
search-action-identity-devices-copy = Copy connected device list
search-action-identity-devices-copy-subtitle = Copy device names, fingerprints, capabilities, and revocation state
search-action-identity-device-enroll = Enroll another device
search-action-identity-device-enroll-subtitle = Copy a device setup code that is valid for five minutes
search-action-identity-home-peer-enroll = Pair a home server
search-action-identity-home-peer-enroll-subtitle = Copy a five-minute setup code with always-on full-sync access
search-action-quick-settings-open = Open quick settings
search-action-quick-settings-open-subtitle = Brightness, volume, Wi-Fi, Bluetooth, power profile
search-action-quick-settings-clipboard = Open clipboard history
search-action-quick-settings-clipboard-subtitle = Search, pin, remove, or reuse private clipboard items
search-action-quick-settings-devices = Open devices and media
search-action-quick-settings-devices-subtitle = Certified devices, media routes, remote input, and handoffs
search-action-projection-open = Choose where the desktop appears
search-action-projection-open-subtitle = Extend, mirror, one screen, or a Miracast, Google Cast, AirPlay or FCast receiver
search-action-projection-close = Close the display sheet
search-action-projection-close-subtitle = Put away the layout and receiver choices; displays stay as they are
search-action-display-layout-extend = Extend displays
search-action-display-layout-extend-subtitle = Use every connected display as one larger desktop
search-action-display-layout-mirror = Mirror displays
search-action-display-layout-mirror-subtitle = Show the same picture on every connected display
search-action-display-layout-builtin-only = Show only on this screen
search-action-display-layout-builtin-only-subtitle = Turn external displays off and keep the built-in screen
search-action-display-layout-external-only = Show only on external displays
search-action-display-layout-external-only-subtitle = Turn the built-in screen off and use the external displays
search-action-display-change-keep = Keep display settings
search-action-display-change-keep-subtitle = Confirm the display layout that was just applied
search-action-display-change-revert = Revert display settings
search-action-display-change-revert-subtitle = Restore the display layout from before the last change
search-action-display-outputs-inspect = Inspect displays
search-action-display-outputs-inspect-subtitle = Scale, position and mode of each display
search-action-bluetooth-pairing-decline = Decline Bluetooth pairing
search-action-bluetooth-pairing-decline-subtitle = Refuse the pairing request on screen, or stop the pairing whose code it shows
search-action-capture-open = Screenshot or float part of the screen
search-action-capture-open-subtitle = Screenshot, record, or make PiP from one or more regions
search-action-collaborators-open = Show collaborators
search-action-collaborators-open-subtitle = Collaborators currently in this workspace
search-action-presence-follow-stop = Stop following
search-action-presence-follow-stop-subtitle = Return this view to local navigation
search-action-presence-wave = Wave
search-action-presence-wave-subtitle = Wave at someone here; their face says it was you
search-action-presence-availability-set = Set your availability
search-action-presence-availability-set-subtitle = Available, Focusing or Away, beside your face
search-action-presence-visibility-set = Choose what people here see
search-action-presence-visibility-set-subtitle = Where you are, that you're here, or nothing
search-action-room-activity-open = Show workspace activity
search-action-room-activity-open-subtitle = History of this workspace: changes, permissions, who did what and when
search-action-peer-invite = Invite a collaborator…
search-action-peer-invite-subtitle = Review an invite before creating it
search-action-peer-connect = Join a workspace…
search-action-peer-connect-subtitle = Public workspaces, a site like tilo.coffee, a link or an invitation
search-action-workspace-publish = Make this workspace public…
search-action-workspace-publish-subtitle = List it in its host's directory with a description and the role people who join get
search-action-workspace-unpublish = Make this workspace private
search-action-workspace-unpublish-subtitle = Take it out of the directory; its members stay
search-action-workspace-report = Report this workspace…
search-action-workspace-report-subtitle = Tell the people who run this public workspace what is wrong
search-action-bot-add = Add a bot to this workspace…
search-action-bot-add-subtitle = Its host runs a bot from the directory here
search-action-bot-remove = Remove a bot from this workspace…
search-action-bot-remove-subtitle = Its host stops the bot and removes it
bots-add-failed = The bot wasn't added
bots-remove-failed = The bot wasn't removed
search-action-pair-device = Pair a new device
search-action-pair-device-subtitle = Make your phone, a browser or another computer one of your devices
search-action-tab-new-terminal-here = New terminal tab here
search-action-tab-new-terminal-here-subtitle = Open a terminal as a tab in the focused window group
search-action-window-new-terminal = New terminal window
search-action-window-new-terminal-subtitle = Open a terminal beside the focused window
search-action-tab-duplicate = Duplicate this tab
search-action-tab-duplicate-subtitle = Open a second instance of the focused app beside it
search-action-tab-close = Close this tab
search-action-tab-close-subtitle = Close the focused tab; its window closes with it if it was the last
search-action-tab-reopen = Reopen closed tab
search-action-tab-reopen-subtitle = Bring back the tab closed most recently in this workspace, where it was
search-action-tab-open-beside = Open an app in a new tab
search-action-tab-open-beside-subtitle = Open another instance of an app, optionally at an address, as a tab right after the focused one
search-action-tab-split-right = Split this tab to the right
search-action-tab-split-right-subtitle = Move the focused tab into a new window group beside its current one
search-action-tab-move-to-new-group = Move this tab to a new group
search-action-tab-move-to-new-group-subtitle = Move the focused tab into a new window group at the end of its strip
search-action-window-focus-previous = Focus the previous window
search-action-window-focus-previous-subtitle = Move focus to the window on the left along the strip
search-action-window-focus-next = Focus the next window
search-action-window-focus-next-subtitle = Move focus to the window on the right along the strip
search-action-window-focus-nth = Focus a window by position
search-action-window-focus-nth-subtitle = Move focus to the Nth window group of the focused strip
search-action-strip-focus-nth = Go to a strip by number
search-action-strip-focus-nth-subtitle = Move focus to the Nth strip of this workspace, counting from the top
search-action-strip-focus-previous = Go to the strip above
search-action-strip-focus-previous-subtitle = Move focus to the strip above in this workspace
search-action-strip-focus-next = Go to the strip below
search-action-strip-focus-next-subtitle = Move focus to the strip below in this workspace
search-action-workspace-previous = Go to the previous workspace
search-action-workspace-previous-subtitle = Switch to the workspace before this one
search-action-workspace-next = Go to the next workspace
search-action-workspace-next-subtitle = Switch to the workspace after this one
search-action-tab-previous = Previous tab
search-action-tab-previous-subtitle = Focus the previous tab in the focused window group
search-action-tab-next = Next tab
search-action-tab-next-subtitle = Focus the next tab in the focused window group
search-action-group-move-left = Move window group left
search-action-group-move-left-subtitle = Move the focused window group one place left in its strip
search-action-group-move-right = Move window group right
search-action-group-move-right-subtitle = Move the focused window group one place right in its strip
search-action-group-move-to-strip-previous = Move window group to the strip above
search-action-group-move-to-strip-previous-subtitle = Carry the focused window group into the strip above
search-action-group-move-to-strip-next = Move window group to the strip below
search-action-group-move-to-strip-next-subtitle = Carry the focused window group into the strip below
search-action-group-move-to-display-previous = Move window group to the previous display
search-action-group-move-to-display-previous-subtitle = Move the focused window group onto the display to the left
search-action-group-move-to-display-next = Move window group to the next display
search-action-group-move-to-display-next-subtitle = Move the focused window group onto the display to the right
search-action-group-maximize = Maximize or restore the window group
search-action-group-maximize-subtitle = Make the focused window group fill its display, or restore it
search-action-group-restore = Restore the window group
search-action-group-restore-subtitle = Return the focused maximized window group to its place in the strip
search-action-group-fullscreen = Full screen or restore
search-action-group-fullscreen-subtitle = Show the focused window full screen, or restore it
search-action-group-narrower = Make the window group narrower
search-action-group-narrower-subtitle = Shrink the focused window group by one step
search-action-group-wider = Make the window group wider
search-action-group-wider-subtitle = Widen the focused window group by one step
search-action-tab-move-to-group-previous = Move this tab into the group on the left
search-action-tab-move-to-group-previous-subtitle = Move the focused tab into the neighbouring window group on the left
search-action-tab-move-to-group-next = Move this tab into the group on the right
search-action-tab-move-to-group-next-subtitle = Move the focused tab into the neighbouring window group on the right
search-action-notifications-dnd-toggle = Do Not Disturb
search-action-notifications-dnd-toggle-subtitle = Hold notification banners until you’re back
search-action-appearance-theme-dark = Dark theme
search-action-appearance-theme-dark-subtitle = Dark chrome and apps
search-action-appearance-theme-light = Light theme
search-action-appearance-theme-light-subtitle = Light chrome and apps
search-action-appearance-high-contrast = High contrast
search-action-appearance-high-contrast-subtitle = More contrast in the current theme
search-action-appearance-language = Change language
search-action-appearance-language-subtitle = The desktop’s language, in Settings
search-action-identity-connect = Add an existing identity
search-action-identity-connect-subtitle = Import identity credentials and use them in this workspace
search-action-identity-select = Use identity in this workspace
search-action-identity-select-subtitle = Select one of the identities already connected to this device
search-action-identity-device-pair-resource = Nearby device pairing
search-action-identity-device-pair-discover = Find a nearby device to pair
search-action-identity-device-pair-discover-subtitle = Look on this network for a new device or home server waiting to join
search-action-identity-device-pair-approve = Approve a nearby device
search-action-identity-device-pair-approve-subtitle = Let the device found nearby join once both screens show the same code
search-action-identity-device-pair-reject = Reject a nearby device
search-action-identity-device-pair-reject-subtitle = Forget a device found nearby and stop approving it
search-action-identity-device-revoke = Revoke an identity device
search-action-identity-device-revoke-subtitle = Permanently stop trusting a lost, retired, or compromised device
search-action-identity-resource = Your identities
search-action-identity-list = List your identities
search-action-identity-list-subtitle = Show the identities on this device and which one new apps sign in as
search-action-identity-activate = Use an identity for new apps
search-action-identity-activate-subtitle = Sign apps you open from now on in as one of your identities
search-action-identity-remove = Remove an identity from this device
search-action-identity-remove-subtitle = Delete an identity you no longer use here, with its key
search-action-identity-protect = Protect an identity's key
search-action-identity-protect-subtitle = Move an identity's key from a plain file into protected storage
search-action-identity-devices-list = List your trusted devices
search-action-identity-devices-list-subtitle = Show the devices trusted under this workspace's identity and which are revoked
search-action-identity-home-peer-remove = Remove a home server
search-action-identity-home-peer-remove-subtitle = Revoke a paired home server and stop it serving your workspaces
search-action-identity-home-peer-retry = Finish removing home servers
search-action-identity-home-peer-retry-subtitle = Tell removed home servers that could not be reached before to stop
search-action-capture-screenshot-send = Send the last screenshot
search-action-capture-screenshot-send-subtitle = To one of your devices, or into this workspace's files
search-action-presence-follow = Follow a collaborator
search-action-presence-follow-subtitle = Keep this view attached to a connected person or visible agent
search-action-presence-status-set = Set workspace status
search-action-presence-status-set-subtitle = Publish an explicit status, or clear it with an empty value
search-action-presence-status-sharing-set = Set status privacy
search-action-presence-status-sharing-set-subtitle = Choose whether status is off, explicit-only, or automatically inferred
search-action-peer-invite-named = Name collaborator and open invite
search-action-peer-invite-named-subtitle = Record the display name, then review their invite before creating it
search-action-member-role-set = Change a member's role
search-action-member-role-set-subtitle = Make a workspace member a viewer, commenter, or editor
search-action-member-remove = Remove a member
search-action-member-remove-subtitle = Take a member out of this workspace and revoke everything they could do here
search-action-invitation-issue = Create an invitation
search-action-invitation-issue-subtitle = Mint an invitation to this workspace with a role and a lifetime
search-action-invitation-list = List pending invitations
search-action-invitation-list-subtitle = Show the invitations to this workspace nobody has redeemed yet
search-action-invitation-resend = Resend an invitation
search-action-invitation-resend-subtitle = Get a pending invitation's token again, without extending it
search-action-invitation-cancel = Cancel an invitation
search-action-invitation-cancel-subtitle = Revoke a pending invitation so it can no longer be redeemed
search-action-agent-invite = Invite agent to workspace
search-action-agent-invite-subtitle = Start a scoped agent setup; choose every surface it may use
search-action-agent-remove = Remove an agent
search-action-agent-remove-subtitle = Take an agent out of this workspace and revoke everything it could do here, at once
search-action-record-bring = Add an item to this workspace
search-action-record-bring-subtitle = Make it available here without moving the original
search-action-quick-settings-brightness-set = Brightness
search-action-quick-settings-brightness-set-subtitle = Set how bright a screen is
search-brightness-display = Brightness, { $display }
search-brightness-display-subtitle = Set how bright this screen is
search-action-quick-settings-volume-set = Volume
search-action-quick-settings-volume-set-subtitle = Set the sound output level
search-action-quick-settings-wifi-set-enabled = Set Wi-Fi enabled state
search-action-quick-settings-wifi-set-enabled-subtitle = Enable or disable this device's Wi-Fi radio
search-action-quick-settings-power-profile-set = Power mode
search-action-quick-settings-power-profile-set-subtitle = Saver, balanced or performance
search-action-quick-settings-vpn-set-enabled = Set VPN enabled state
search-action-quick-settings-vpn-set-enabled-subtitle = Enable or disable one named VPN connection on this device
search-action-quick-settings-bluetooth-set-enabled = Set Bluetooth enabled state
search-action-quick-settings-bluetooth-set-enabled-subtitle = Power this device's Bluetooth radio on or off
search-action-quick-settings-airplane-mode-set-enabled = Set airplane mode
search-action-quick-settings-airplane-mode-set-enabled-subtitle = Turn every wireless radio on this device off or back on

# Search: search sources and workspace governance rows
search-action-search-provider-set = Enable or disable a search source
search-action-search-provider-set-subtitle = Customize which providers appear in search and agent results
search-action-workspace-switch-subtitle = Go to this workspace and its windows
search-action-activity-catch-up-subtitle = What others changed since you last looked
search-action-activity-tasks-subtitle = Work assigned to people and agents here
search-action-activity-explore-subtitle = Every change here, by person, app and time
search-action-activity-replay-subtitle = Step through how this workspace changed
search-action-quick-settings-unavailable = Quick Settings service is unavailable
# Extra words a search for a workspace switch or Activity view matches; not shown.
search-action-workspace-switch-keywords = switch workspace go
search-action-activity-keywords = activity history changes timeline who

search-action-history-compare = Compare this change
search-action-history-compare-subtitle = Diff the selected activity row
search-action-history-compare-keywords = compare diff history change
search-action-history-undo = Undo this change
search-action-history-undo-subtitle = Reverse the selected activity row
search-action-history-undo-keywords = undo reverse rückgängig
search-action-history-restore = Restore this version
search-action-history-restore-subtitle = Put the document back to the selected point
search-action-history-restore-keywords = restore version history wiederherstellen
search-action-provider-enable = Enable { $source } search
search-action-provider-enable-subtitle = Show { $source } results in search and agent discovery
search-action-provider-disable = Disable { $source } search
search-action-provider-disable-subtitle = Hide { $source } results from search and agent discovery
search-action-provider-badge-on = ON
search-action-provider-badge-off = OFF
# Words that ask for the per-source search on/off switches; not shown.
search-action-provider-keywords = search source sources disable enable
search-action-governance-subtitle = Signed workspace governance action for the active workspace
search-action-permission-roles-subtitle = Manage reusable least-privilege workspace roles

# Search: what a shell action did
search-action-tab-close-failed = Can't close this tab
search-action-tab-split-failed = Can't split this tab
search-action-device-list-copied = Device list copied
search-action-enrollment-copied = Device setup code copied. It expires in { $minutes } min.
search-action-home-server-copied = Home-server setup code copied. It expires in { $minutes } min.
search-action-enrollment-copy-failed = Could not copy the setup code to the clipboard
search-action-device-revoked = Device revoked
search-action-device-revoked-body = The revocation was root-signed and recorded in this identity's device list.

# Search: applications
search-apps-shortcut-subtitle = Shortcut
search-apps-shortcut-failed = The shortcut could not be opened. Check its target and try again.

# Search: apps to get, from the catalogue search shares with the App Store
search-catalogue-provider = App Store
search-catalogue-installed-provider = App actions
search-catalogue-source-tilo = Tilo app
search-catalogue-source-flathub = Flathub
search-catalogue-document-subtitle = New in { $app }
search-catalogue-show-in-store = Show in App Store
search-catalogue-store-failed = Couldn't open the App Store for { $app }
search-catalogue-installing = Installing { $app }
search-catalogue-installing-body = It opens when it's installed.
search-catalogue-actions-unavailable = Actions aren't available yet. Try again in a moment.

# Search: continuity actions across your certified devices
search-continuity-certified-device = certified device
search-continuity-play-on = Play { $source } on { $target }
search-continuity-wake-device = Wake a device
search-continuity-wake-device-subtitle = Sends a wake-up signal from a device on its network
device-wake-app-name = Devices
device-wake-rejoined = { $device } is awake
device-wake-not-rejoined = { $device } did not wake up
device-wake-not-rejoined-body = It got a wake-up signal but has not come back. It may be off or out of power.
device-wake-failed = { $device } could not be woken
device-wake-failed-body = No wake-up signal could be sent on its network.
search-continuity-media-handoff-subtitle = Move this live media session to an exact certified device
search-continuity-media-cancel = Cancel moving { $media }
search-continuity-media-retry = Retry moving { $media }
search-continuity-media-status-subtitle = Media handoff to { $device }
search-continuity-bluetooth-allow = Allow handoff of { $accessory } from { $device }
search-continuity-bluetooth-decline = Decline handoff of { $accessory } from { $device }
search-continuity-bluetooth-consent-subtitle = Resolve this exact Bluetooth pairing handoff request
search-continuity-bluetooth-retry = Retry handoff of { $accessory }
search-continuity-bluetooth-cancel = Cancel handoff of { $accessory }
search-continuity-bluetooth-status-subtitle = Bluetooth continuity with { $device }
search-continuity-route-audio = Route audio to { $output }
search-continuity-route-audio-subtitle = Choose an exact currently enumerated local audio output
search-continuity-input-raise = Raise the input level of { $input }
search-continuity-input-lower = Lower the input level of { $input }
search-continuity-input-level-subtitle = Change the current microphone's level by one step
search-continuity-input-select = Use { $input } as the microphone
search-continuity-input-select-subtitle = Record from this local input from now on
search-continuity-audio-forward-subtitle = Request a consented live-audio route to this certified device
search-continuity-audio-allow = Allow live audio from { $device }
search-continuity-audio-decline = Decline live audio from { $device }
search-continuity-audio-consent-subtitle = Resolve this exact remote-audio consent request
search-continuity-audio-stop = Stop routing { $source }
search-continuity-audio-stop-subtitle = Close and neutralize this exact live-audio route
search-continuity-speaker-join = Join { $group } as the { $role }
search-continuity-speaker-decline = Decline to join { $group } as the { $role }
search-continuity-speaker-consent-subtitle = { $device } asked this device to play in a speaker group
search-continuity-speaker-leave = Leave { $group }
search-continuity-speaker-leave-subtitle = Stop playing as the { $role } in this speaker group
search-continuity-speaker-play-subtitle = Play through every ready speaker of this group
search-continuity-speaker-media-subtitle = Keep playing here and send the sound to this speaker group
search-continuity-speaker-stop = Stop playing on { $group }
search-continuity-speaker-stop-subtitle = End this speaker group playback on every speaker
search-continuity-speaker-listen-allow = Let { $group } calibrate with this microphone
search-continuity-speaker-listen-decline = Do not let { $group } use this microphone
search-continuity-speaker-listen-subtitle = { $device } asked to listen with this device's microphone while calibrating speakers
search-continuity-speaker-listen-stop = Stop { $group } using this microphone
search-continuity-speaker-listen-stop-subtitle = Speaker calibration set up on { $leader } may listen with this microphone
search-continuity-place-left = Place { $device } left
search-continuity-place-right = Place { $device } right
search-continuity-place-above = Place { $device } above
search-continuity-place-below = Place { $device } below
search-continuity-place-subtitle = Arrange an exact certified display around this device
search-continuity-control-revoke = Revoke control from { $device }
search-continuity-control-grant = Grant control to { $device }
search-continuity-control-subtitle = Change the short-lived cross-device input lease
search-continuity-calibration-apply = Apply placement suggestion for { $device }
search-continuity-calibration-dismiss = Dismiss placement suggestion for { $device }
search-continuity-calibration-subtitle = Act on the current proximity-derived placement proposal
search-continuity-calibration-revert = Revert placement for { $device }
search-continuity-calibration-revert-subtitle = Restore the previous certified-display placement
search-continuity-tether-request = Ask { $device } for tethering
search-continuity-tether-request-subtitle = Request a short-lived, phone-approved connection
search-continuity-unlock-approve = Unlock { $device } (code { $code })
search-continuity-unlock-deny = Keep { $device } locked (code { $code })
search-continuity-unlock-subtitle = It restarted and is asking to open its encrypted disk
search-continuity-device-remove = Remove { $device } from your devices
search-continuity-device-remove-subtitle = It can no longer act for you or unlock its disk through your devices
device-unlock-error-not-pending = That unlock request is no longer waiting
device-unlock-error-cannot-remove = This device cannot remove that device
device-unlock-error-no-identity = This device does not hold your identity
search-continuity-tether-allow = Allow tethering for { $device }
search-continuity-tether-decline = Decline tethering for { $device }
search-continuity-tether-consent-subtitle = Resolve this exact phone-side consent request
search-continuity-tether-disconnect = Disconnect tethering from { $device }
search-continuity-tether-disconnect-subtitle = Revoke this exact tethering request and its network route
search-continuity-capture-camera = Use camera from { $device }
search-continuity-capture-microphone = Use microphone from { $device }
search-continuity-capture-scanner = Use scanner from { $device }
search-continuity-capture-request-subtitle = Request a one-session remote input for the focused app
search-continuity-capture-allow = Allow remote input for { $app }
search-continuity-capture-decline = Decline remote input for { $app }
search-continuity-capture-consent-subtitle = Resolve this exact source-side capture request
search-continuity-capture-approve-app = Allow focused app from { $device }
search-continuity-capture-stop = Stop remote input from { $device }
search-continuity-capture-session-subtitle = Act on this exact remote capture session
search-continuity-controller-stop = Stop forwarding { $controller }
search-continuity-controller-stop-subtitle = Neutralize and close this controller session
search-continuity-controller-start = Use { $controller } on { $device }
search-continuity-controller-start-subtitle = Forward normalized controls to the currently focused app
search-continuity-controller-allow = Allow { $controller } from { $device }
search-continuity-controller-decline = Decline { $controller } from { $device }
search-continuity-controller-consent-subtitle = Controller access for the locally verified focused app
search-continuity-call-move = Move { $room } call to { $device }
search-continuity-call-move-subtitle = Propose handoff to an exact certified device
search-continuity-call-allow = Allow handoff for { $room }
search-continuity-call-decline = Decline handoff for { $room }
search-continuity-call-consent-subtitle = Resolve this exact call handoff consent

# First-login identity setup: what the identity is
onboarding-intro-title = Set up your Tilo identity
onboarding-intro-body = You get a cryptographic identity that belongs to you, and this device gets its own key. Collaboration and invitations rely on it. Not every edit in apps is signed yet.
onboarding-intro-point-collaboration = Collaborators see one stable identity for you, and it vouches for each of your devices.
onboarding-intro-point-local = It is created on this computer. Nothing is uploaded and no server keeps a copy.
onboarding-intro-point-not-account = It is not an account and has no password. There is nothing to sign in to.
onboarding-choice-create = Create a new identity
onboarding-choice-create-detail = Start fresh on this computer
onboarding-choice-existing = Use my existing Tilo identity
onboarding-choice-existing-detail = Bring it over from another device or a backup
onboarding-host-intro-title = Set up your Tilo identity
onboarding-host-intro-body = You are already signed in to this computer. Before the Tilo Desktop opens, it needs a local cryptographic identity for collaboration and peer invitations. This does not replace your Linux login or create an online account. Individual app edits are not all cryptographically signed yet.

# First-login identity setup: no recovery service
onboarding-no-recovery-title = If you lose this device without a backup, this identity is gone
onboarding-no-recovery-point-no-service = There is no recovery service. No backup code and no recovery phrase can recreate this identity.
onboarding-no-recovery-point-backup = The encrypted backup offered next is the only recovery path. Keep its file and passphrase separately. Without it, a lost or wiped device needs a new identity; workspace data remains, but the old identity is no longer yours.
onboarding-acknowledgement = I understand that losing this device without a backup means losing this identity.
onboarding-protect-title = Protect your Tilo identity
onboarding-protect-message = There is no reset button. Create an encrypted portable backup after this step.
onboarding-create-failed = Your identity was not created
onboarding-create-failed-title = Your identity could not be created
onboarding-create-failed-message = Nothing was written and the desktop is still protected. Go back and try again after checking storage and permissions.

# First-login identity setup: progress header
onboarding-progress-eyebrow = Tilo identity setup
onboarding-progress-stage = Step { $step } of { $total }
onboarding-progress-a11y = Identity setup, step { $step } of { $total }: { $stage }
onboarding-stage-choose = Choose
onboarding-stage-restore = Restore
onboarding-stage-connect = Connect
onboarding-stage-protect = Protect
onboarding-stage-media = Media
onboarding-stage-ready = Ready
onboarding-stage-backup = Backup

# First-login identity setup: resuming after a restart
onboarding-progress-attention = Setup progress needs attention
onboarding-progress-unreadable = The saved setup position could not be read ({ $error }). Your identity is safe; setup resumed at the completion step.
onboarding-progress-invalid-identity = Setup found unfinished progress but could not validate the existing identity: { $error }
onboarding-progress-save-failed = Setup could not save its restart position: { $error }. No identity action was repeated.
onboarding-progress-clear-failed = Setup finished, but its restart marker could not be removed: { $error }. Try Finish again so this screen does not return next login.

# First-login identity setup: shared buttons
onboarding-back = Back
onboarding-cancel = Cancel
onboarding-continue = Continue
onboarding-done = Done
onboarding-create-local-identity = Create local identity
onboarding-restore-backup = Restore identity
onboarding-existing-title = Use your existing identity
onboarding-existing-body = Choose how to bring the identity you already use to this computer.
onboarding-choice-nearby = Connect a nearby device
onboarding-choice-nearby-detail = Approve this computer from a device that already has your identity
onboarding-choice-restore = Restore from a backup
onboarding-choice-restore-detail = Open an encrypted identity backup file
onboarding-restore-title = Restore from a backup
onboarding-restore-body = Choose the encrypted backup you saved from Tilo and enter its passphrase.
onboarding-restore-file-label = Backup file

# First-login identity setup: connecting an existing identity
onboarding-connect-recovery-title = Your existing identity needs recovery
onboarding-connect-recovery-message = Your existing identity couldn't be verified. Unlock the desktop keyring and try again, or restore an encrypted backup. Your identity stays as it is until then.
onboarding-connect-failed = That identity could not be connected
onboarding-connect-retry-keyring = Retry keyring
onboarding-connect-browse = Browse
onboarding-connect-file = Identity backup file
onboarding-connect-file-placeholder = Choose an encrypted backup or legacy identity JSON
onboarding-connect-passphrase = Backup passphrase
onboarding-connect-passphrase-placeholder = Passphrase (leave empty for legacy JSON)
onboarding-connect-choose-file-first = Choose an encrypted backup or legacy identity file first.
onboarding-keyring-unavailable = The desktop keyring is still unavailable. Unlock it and retry, or restore an encrypted backup: { $error }
onboarding-picker-import-title = Choose a Tilo identity
onboarding-picker-import-filter = Tilo identity
onboarding-nearby-title = Connect a nearby device
onboarding-nearby-body = This computer is waiting for your other device to approve it. Keep both devices close together.
onboarding-nearby-step-open = On your other device, open Settings and go to Devices & Sync.
onboarding-nearby-step-add = Under Add a personal device, choose Add device.
onboarding-nearby-step-compare = Compare the six-digit code on both screens, then approve it on your other device.
onboarding-nearby-waiting = Waiting for your other device.
onboarding-nearby-code-label = Verification code
onboarding-nearby-try-again = Try again

# First-login identity setup: pairing with a nearby device
onboarding-nearby-failed-title = Pairing did not finish
onboarding-nearby-ended = The pairing window closed or approval was cancelled. Choose Try again.
onboarding-nearby-unknown = Choose Try again.
onboarding-nearby-detail = Details: { $detail }
onboarding-nearby-permission-title = Permission denied: this computer was not allowed to save the pairing
onboarding-nearby-permission = This computer's device identity couldn't be saved. Once that's fixed, choose Try again.
onboarding-nearby-storage-full-title = This computer's disk is full
onboarding-nearby-storage-full = The pairing could not be saved. Free up some space, then choose Try again.
onboarding-nearby-read-only-title = This computer's storage is read-only
onboarding-nearby-read-only = The pairing could not be saved. Choose Try again once the storage can be written to.
onboarding-nearby-unreachable-title = Your other device could not be reached
onboarding-nearby-unreachable = Check that both devices are on the same network, then choose Try again.
onboarding-nearby-timed-out-title = No trusted device approved this computer in time
onboarding-nearby-timed-out = Choose Try again, then approve it on your other device.
onboarding-nearby-invalid-identity-title = Your other device returned an invalid identity
onboarding-nearby-invalid-identity = Choose Try again.
onboarding-nearby-not-started-title = Nearby setup could not start
onboarding-nearby-not-started = Choose Try again.
onboarding-nearby-unavailable = Nearby setup is unavailable in this session
onboarding-nearby-cancelled = Pairing cancelled. Choose Try again to start over.
onboarding-nearby-code = Check that your other device shows the same code, then approve it there.
onboarding-nearby-connected = Connected. Your workspaces will appear automatically.

# First-login identity setup: encrypted backup
onboarding-backup-title = Back up your Tilo identity
onboarding-backup-identity-created = Your local identity was created
onboarding-backup-explanation = Save an encrypted copy somewhere else. Keep its file and passphrase separately; together they restore this identity.
onboarding-backup-passphrase = New backup passphrase
onboarding-backup-passphrase-placeholder = At least 12 characters, long and unique
onboarding-backup-confirmation = Confirm new backup passphrase
onboarding-backup-confirmation-placeholder = Confirm passphrase
onboarding-backup-save = Choose location and save backup…
onboarding-backup-save-another = Save another copy
onboarding-backup-save-action = Save an encrypted backup
onboarding-backup-skip-acknowledgement = I understand that without an encrypted backup, losing this device means losing this identity.
onboarding-backup-continue-without = Continue without backup
onboarding-backup-failed = The backup was not saved
onboarding-backup-verified = Encrypted backup verified
onboarding-backup-verified-earlier = The verified backup was saved before setup restarted.
onboarding-passphrase-too-short = Use at least 12 characters. Make it long and unique; length alone does not make a common passphrase safe.
onboarding-passphrase-mismatch = The two passphrases do not match.
onboarding-passphrase-unavailable = The passphrase fields are temporarily unavailable.
onboarding-picker-backup-title = Save encrypted identity backup
onboarding-picker-backup-filter = Encrypted Tilo identity

# First-login identity setup: review renderer fixtures
onboarding-snapshot-restore-error = That backup could not be opened. Check the file and passphrase, then try again.
onboarding-snapshot-create-error = The identity could not be saved. Nothing was created; check available storage and try again.

# First-login identity setup: identity ready
onboarding-done-title = Your identity is ready
onboarding-done-body = This fingerprint identifies your local cryptographic root. Tilo can present it in collaboration flows that support verification; you do not need to type it.
onboarding-done-ready = Identity stored on this device
onboarding-host-done-title = Your Tilo identity is ready
onboarding-host-done-body = You use this local identity when you collaborate or invite people. It is separate from your Linux login and is not an online account.
onboarding-host-done-ready = Ready for Tilo collaboration
onboarding-done-next-title = What happens next
onboarding-done-next-body = The desktop is ready. Sharing remains off until you deliberately enable it in Settings.
onboarding-done-backup-verified = Backup verified
onboarding-done-backup-verified-body = Keep the encrypted backup file and its passphrase in separate places.
onboarding-done-backup-recommended = Backup still recommended
onboarding-done-backup-recommended-body = Create an encrypted portable backup from Settings before relying on this identity.
onboarding-done-joined-key-elsewhere = Nothing to back up here
onboarding-done-joined-key-elsewhere-body = This device joined your identity from another device, which keeps its key. Losing this device loses no identity; keep the backup of the device that approved it.
onboarding-done-system-care-title = System care
onboarding-done-system-care-body = Signed system updates are checked automatically when the computer is online. You can check, stage, and roll back releases in Settings.
onboarding-tour-take = Take a quick tour
onboarding-finish = Continue to desktop

# First-login setup: more video and audio formats
onboarding-codecs-title = Play more video and audio formats
onboarding-codecs-note = Downloaded to this device from the formats' publishers. You can turn it off in Settings.
onboarding-codecs-background = Setup can continue while the download runs. You will get a notification when it is ready.
onboarding-codecs-progress = Progress: { $percent }%
onboarding-codecs-starting = Starting…
onboarding-codecs-installing-title = Installing more formats
onboarding-codecs-installing-message = { $status } Setup can continue while the download runs. You will get a notification when it is ready.
onboarding-codecs-offline-title = Installs once you are online
onboarding-codecs-offline-message = This device is not connected to the internet. Your choice is saved: the download starts by itself once it is online, and you will get a notification.
onboarding-codecs-installed-title = More video and audio formats are on
onboarding-codecs-failed-title = Could not install more formats
onboarding-codecs-failed-message = Try again, or later from Settings, Updates.
onboarding-codecs-failed-message-detail = { $error } Try again, or later from Settings, Updates.
onboarding-codecs-installer-failed = The download could not start: { $error }
onboarding-codecs-skip = Skip
onboarding-codecs-not-now = Not now
onboarding-codecs-retry = Try again
onboarding-codecs-install = Install

# Desktop tour
onboarding-tour-eyebrow = Desktop tour
onboarding-tour-stage = Step { $step } of 4
onboarding-tour-progress-a11y = Desktop tour, step { $step } of 4: { $stage }
onboarding-tour-stage-move = Move
onboarding-tour-stage-arrange = Arrange
onboarding-tour-stage-share = Share
onboarding-tour-stage-begin = Begin
onboarding-tour-skip = Skip tour
onboarding-tour-enter = Enter Tilo
onboarding-tour-navigate-title = Everything is close at hand
onboarding-tour-navigate-intro = The desktop stays calm until you ask for something. Every gesture here has a keyboard route too.
onboarding-tour-search-title = Search
onboarding-tour-search-title-mac = Search
onboarding-tour-search-keys = Super+Space
onboarding-tour-search-keys-mac = Command+K or Option+Space
onboarding-tour-search-body = Open it without typing to browse Start here, Everyday, Work, Media, and System tools; type to find any app, file, setting, or action.
onboarding-tour-overview-title = Overview
onboarding-tour-overview-keys = Super+Tab or pinch
onboarding-tour-overview-body = See every workspace and window group, then use Up, Down, and Enter to choose one.
onboarding-tour-move-title = Move
onboarding-tour-move-keys = Super+arrow keys or three-finger swipe
onboarding-tour-move-body = Left and right step between tiled windows along the current strip; up maximizes the window and down restores it. Ctrl+Super+Up and Down, or a three-finger swipe up or down, go to the strip above or below, and Super+1 to 9 straight to a strip; Super+Page Up and Page Down, or a four-finger swipe, switch workspaces.
onboarding-tour-organise-title = A place for each kind of work
onboarding-tour-organise-intro = The layout is simple once its four levels have names. Tilo remembers the arrangement for you.
onboarding-tour-workspace-title = Workspace
onboarding-tour-workspace-body = A durable place for a project, its people, permissions, apps, and history.
onboarding-tour-strip-title = Strip
onboarding-tour-strip-body = A horizontal run of window groups inside a workspace. Add another strip when work needs a second lane.
onboarding-tour-window-group-title = Window group
onboarding-tour-window-group-body = A tiled frame for tabs that belong together. Move between groups with Super+Left or Super+Right.
onboarding-tour-tab-title = Tab
onboarding-tour-tab-body = An app or document inside a window group. Drag tabs to arrange them without overlapping windows.
onboarding-tour-collaborate-title = Sharing stays deliberate
onboarding-tour-collaborate-intro = Collaboration is built into the workspace, not bolted onto individual apps. Nothing becomes shared merely because it is open.
onboarding-tour-invite-settings-title = Invite from Workspace Settings
onboarding-tour-invite-settings-body = Choose a role and review exactly which workspace the invitation opens.
onboarding-tour-follow-title = Follow only when you choose
onboarding-tour-follow-body = People keep their own focus and view. Follow is an explicit, reversible action.
onboarding-tour-system-features-title = Undo and permissions are system features
onboarding-tour-system-features-body = History follows authorship, and apps ask Tilo for capabilities instead of receiving ambient access.
onboarding-tour-start-title = Your first five minutes
onboarding-tour-start-intro = Start with one outcome, then let the workspace grow with the project. Find this again from Search as Desktop tour.
onboarding-tour-name-title = Name the workspace
onboarding-tour-name-body = Open Workspace Settings from the title and choose a recognisable project name.
onboarding-tour-open-title = Open a shared starting point
onboarding-tour-open-body = Choose Notes, Calendar, or Chat, then add other apps beside it.
onboarding-tour-invite-title = Invite the right people
onboarding-tour-invite-body = Choose Invite people in Workspace Settings and review each role before sharing.
onboarding-tour-check-title = Check work and recover mistakes
onboarding-tour-check-body = Open Activity to catch up, compare versions, or undo only the change that went wrong.

# Workspace invite card: inviting someone, joining with an invite, pending invites
peer-sync-title-review = Review invitation
peer-sync-title-invite = Invite someone
peer-sync-title-join = Join a workspace
peer-sync-title-workspace-invite = Workspace invite
peer-sync-paste-token = Type a site, or paste a workspace or invitation link
peer-sync-identity-copied = Copied
peer-sync-invite-failed = Invite failed: { $error }
peer-sync-identity-unavailable = Identity unavailable: { $error }
peer-sync-joined-room = Joined room { $room }
peer-sync-join-refused = Refused: { $error }
peer-sync-pending-unavailable = Pending invites unavailable: { $error }
peer-sync-resend-failed = Could not resend: { $error }
peer-sync-invite-canceled = Invite canceled
peer-sync-cancel-failed = Could not cancel: { $error }
peer-sync-unexpected-response = Unexpected collaboration response
peer-sync-working = Working…
peer-sync-using-verified-identity = Using verified identity: { $verification }
peer-sync-ask-for-identity = Ask them to send their Tilo identity, then paste it here.
peer-sync-name-required = Say who this invite is for
peer-sync-identity-required = Paste their Tilo identity key so the invite cannot be stolen
peer-sync-recipient-required = Choose a verified person or paste their Tilo identity so the invite cannot be stolen
peer-sync-copied = Copied
peer-sync-copy-failed = Could not copy to the clipboard
peer-sync-joining = Joining…
peer-sync-pasted = Pasted
peer-sync-clipboard-empty = Nothing on the clipboard to paste
peer-sync-identity-bound = Identity bound: { $fingerprint }
peer-sync-identity-invalid = That is not a Tilo identity key (expected 64 hexadecimal characters)
peer-sync-pending-heading = Pending invites
peer-sync-pending-heading-paged = Pending invites ({ $first }–{ $last } of { $total })
peer-sync-role-expires = { $role }, { $unit ->
    [now] expires in moments
    [seconds] expires in { $count ->
        [one] 1 second
       *[other] { $count } seconds
    }
    [minutes] expires in { $count ->
        [one] 1 minute
       *[other] { $count } minutes
    }
    [hours] expires in { $count ->
        [one] 1 hour
       *[other] { $count } hours
    }
   *[days] expires in { $count ->
        [one] 1 day
       *[other] { $count } days
    }
}
peer-sync-received-role-expires = { $role } access, { $unit ->
    [now] expires in moments
    [seconds] expires in { $count ->
        [one] 1 second
       *[other] { $count } seconds
    }
    [minutes] expires in { $count ->
        [one] 1 minute
       *[other] { $count } minutes
    }
    [hours] expires in { $count ->
        [one] 1 hour
       *[other] { $count } hours
    }
   *[days] expires in { $count ->
        [one] 1 day
       *[other] { $count } days
    }
}
peer-sync-role-viewer = Viewer
peer-sync-role-commenter = Commenter
peer-sync-role-editor = Editor
peer-sync-copy = Copy
peer-sync-cancel = Cancel
peer-sync-back = Back
peer-sync-join = Join
peer-sync-paste = Paste
peer-sync-create-invite = Create invite
peer-sync-review-invite = Review invite
peer-sync-paste-identity = Paste new identity
peer-sync-copy-identity = Copy my identity
peer-sync-recipient-heading = Who are you inviting?
peer-sync-recipient-new = Someone new
peer-sync-role-heading = Access
peer-sync-review-workspace = Workspace: { $workspace }
peer-sync-review-workspace-this = Workspace: this workspace
peer-sync-review-inviter = Invited by: { $inviter }
peer-sync-review-inviter-you = Invited by: You
peer-sync-review-recipient = Recipient: { $name }
peer-sync-review-recipient-unnamed = Recipient: Someone
peer-sync-review-role = Access: { $role }
peer-sync-review-identity = Bound identity: { $fingerprint }
peer-sync-review-identity-missing = Bound identity: missing
peer-sync-review-note = Only this identity can redeem the signed one-hour invite.
peer-sync-selected-person = Selected person
peer-sync-name-new-person = Name the new person
peer-sync-name-placeholder = Their name
peer-sync-recipient-hint = Choose a verified person or paste their Tilo identity.
peer-sync-identity-remembered = Verified remembered identity: { $fingerprint }
peer-sync-identity-new = New identity bound: { $fingerprint }
peer-sync-invite-anyone = Anyone with this invite
peer-sync-invite-received = Invite received
peer-sync-paste-invite-hint = Like tilo.coffee, or a link someone sent you.
peer-sync-join-site = Joins the workspace at { $host }
peer-sync-join-invitation-link = An invitation to a workspace at { $host }
peer-sync-join-invitation = An invitation to a workspace
peer-sync-token-characters = { $token }, { $count ->
    [one] 1 character
   *[other] { $count } characters
}
peer-sync-token-incomplete = Not a site or link yet

# Leftover shell notices: document opens, host app provenance, quiet status, collaboration
doc-open-failed = Couldn't open { $name }
doc-open-unnamed-file = File
host-app-source-flatpak = Flatpak
host-app-source-system = System
new-document-unavailable = New document unavailable: { $error }
collab-unavailable-title = Collaboration unavailable
collab-unavailable-body = Apps in this session cannot share edits. { $reason }
quiet-status-dnd = Do Not Disturb
quiet-status-dnd-presenting = Do Not Disturb while presenting
quiet-status-dnd-sharing = Do Not Disturb while sharing
quiet-status-focus-until = Focus until { $time }

# Saved workspaces that could not be opened at startup
state-notice-newer-title = Your workspaces were not opened
state-notice-unreadable-title = Your workspaces could not be read
state-notice-newer-body = Saved by a newer version of Tilo. Nothing was changed.
state-notice-unreadable-body = The file could not be read. Nothing was changed.
state-notice-newer-detail = { $path } was written by a newer version of Tilo (layout { $found }; this one understands { $understood }). It has not been changed, and nothing has been written over it — this session runs without saving, so it cannot be. Update again, or roll forward, to get back to your own workspaces.
state-notice-unreadable-detail = { $path } exists but could not be read ({ $error }). It has not been changed, and this session runs without saving, so it cannot be.

# Workspace overview: who is in a room and what it shares
overview-access-shared-one = Shared with { $name }
overview-access-shared-two = Shared with { $first } and { $second }
overview-access-shared-many = { $count ->
    [one] Shared with { $name } and 1 other
   *[other] Shared with { $name } and { $count } others
}
overview-items-shared = { $count ->
    [one] 1 item shared
   *[other] { $count } items shared
}

# Windows and documents the shell hosts
shell-window-restoring = Restoring this window…
shell-window-opening = Opening…
shell-window-opening-slow = Opening { $app }… taking longer than usual
shell-window-installing-slow = Installing { $app }… taking longer than usual
shell-window-installing-of = Installing { $app }… { $received } of { $total }
shell-window-installing-received = Installing { $app }… { $received }
shell-window-updating = Updating { $app }…
shell-window-updating-slow = Updating { $app }… taking longer than usual
shell-window-updating-of = Updating { $app }… { $received } of { $total }
shell-window-updating-received = Updating { $app }… { $received }
peer-window-not-installed = Not installed on this device.
peer-window-install = Install { $app }
peer-window-installing = Installing { $app }…
peer-window-unavailable = { $app } isn't available for this device.
hosted-window-running-on = Running on { $device }
shell-document-unavailable = Document unavailable
shell-named-document-unavailable = { $name } unavailable
shell-path-open-failed = Cannot open { $path }: { $error }
editor-disk-conflict = File changed on disk; unsaved edits are preserved for conflict resolution
editor-hint-disk-conflict = File changed on disk. Your unsaved edits are kept.
editor-hint-focused = Live-shared code  ·  Ctrl+Z to undo
notes-empty-title = This note is empty
notes-empty-body = Jot anything here. Changes are saved as you type.
notes-empty-action = Start writing
window-move-unknown-tab = That window is no longer open.
window-move-unknown-workspace = That workspace is gone.
window-move-same-workspace = The window is already in that workspace.
window-move-pinned = This window belongs to its workspace and cannot move.
window-move-document-refused = Couldn't move the window. Try again.
window-move-hosted-workspace = That workspace is run by its owner, and this device can't reach it right now, so windows can't be carried into or out of it.
window-move-not-permitted = That workspace doesn't let you open this app there.
window-move-already-moving = This window is already being moved.
window-move-nobody-to-confirm = Moving a window needs the person at this desktop to agree to what crosses, and nobody is here to ask.
window-transfer-title = Move “{ $window }” to { $to }?
window-transfer-carries = The document in “{ $window }” goes to { $to } with its full edit history, and the window reopens there where you left it.
window-transfer-move-detail = Move: it leaves { $from }, whose history keeps a note that it moved to { $to }.
window-transfer-copy-detail = Copy: { $to } gets its own copy with the history so far, the original stays in { $from }, and from then on the two change separately.
window-transfer-access-only-you = Only you can open it in { $from }.
window-transfer-access-others = In { $from }, { $people } can open it now. After a move they lose access; after a copy they keep the original.
window-transfer-access-destination = In { $to }, that workspace's members and permissions decide who can open it.
window-transfer-grants-stay = What this app may do in { $from } stays there and doesn't come along: { $grants }.
window-transfer-move-blocked = Move isn't available because another window in { $from } still shows this document. Close that window first, or make a copy.
window-transfer-choice-move = Move — { $from } no longer has it
window-transfer-choice-copy = Copy — { $from } keeps the original
window-transfer-confirm = Move
window-transfer-fresh-title = “{ $window }” can't take its work to { $to }
window-transfer-fresh-detail = This app keeps what you're doing inside its running program, so none of it can cross into another workspace. “{ $window }” stays open in { $from }, unchanged. You can open a new, empty window of the app in { $to } instead.
window-transfer-choice-fresh = Open a new window in { $to }
window-transfer-fresh-confirm = Open
window-transfer-failed-title = Couldn't move “{ $window }”
window-transfer-failed-detail = { $reason } Nothing was lost: the document is still in { $from }.
window-transfer-failed-busy = The app still has the document open.
window-transfer-failed-record = The app's document couldn't be read.
window-transfer-failed-storage = The app's storage couldn't be reached.
window-transfer-failed-timeout = The app didn't open a window in the other workspace in time.
window-transfer-failed-closed = The new window was closed before it opened.
window-transfer-reopen-failed = The document is here, but the app didn't open a window. Open it again to continue.
window-transfer-bringing-edits = Bringing over your last edits…
workspace-allow-app-action = { $lifetime ->
    [once] Allowed { $app } to use { $target }: { $action }, once
    [session] Allowed { $app } to use { $target }: { $action }, for this session
   *[workspace] Allowed { $app } to use { $target }: { $action }, in this workspace
}
shell-copy-window-in = Copied “{ $window }” here from { $from }
shell-move-window-out = Moved “{ $window }” to { $to }
shell-move-window-in = Moved “{ $window }” here from { $from }
window-transfer-lost-title = “{ $window }” closed while it was moving
window-transfer-lost-detail = The app closed unexpectedly, so edits from its last few seconds may be missing. Everything it had saved is now in { $to }.
window-transfer-late-edits-title = Your last edits in “{ $window }” came along
window-transfer-late-edits-detail = Changes made in { $from } while the window was moving were added to the document.
window-transfer-move-needs-window = Move needs the window open in { $from }, so it can mark the original as moved there. Open it first, or make a copy.
window-transfer-owner-accepts = { $to } is run by its owner, who has to accept the document before it opens there.
window-transfer-awaiting-owner = Waiting for the owner of { $to } to accept it…
window-transfer-too-large-hosted = It is too large to move into { $to } yet.
window-transfer-proposal-failed = { $to } didn't take it: { $reason }
window-transfer-declined = The owner of { $to } declined it.
window-transfer-owner-timeout = The owner of { $to } didn't answer in time.
window-transfer-kept-as-copy-title = “{ $window }” was copied, not moved
window-transfer-kept-as-copy-detail = It changed in { $from } while the owner of { $to } decided, so both stay. { $to } has it as it was when you moved it.
window-transfer-not-retired-title = “{ $window }” is still in { $from }
window-transfer-not-retired-detail = Its window closed before it could mark the document as moved, so the original stays in { $from } as well.
window-transfer-late-edits-stay-title = Your last edits stayed in { $from }
window-transfer-late-edits-stay-detail = Changes made in { $from } just before its window closed are in the document's history there, not in { $to }.
document-import-title = Accept “{ $title }” into { $to }?
document-import-detail = { $person } wants to move “{ $title }” ({ $app }) into { $to }, with its history. Everyone in { $to } will be able to open it.
document-import-accept = Accept it into { $to }
document-import-decline = Decline
document-import-confirm = Answer
document-import-someone = Someone
document-import-answer-failed-title = Your answer about “{ $title }” didn't reach its workspace
window-transfer-moved-away-title = “{ $window }” was moved to another workspace
window-transfer-moved-away-to = { $person } moved it to { $to }, where it opens for you now. What you see here is how it was when it left; it can't be edited here any more.
window-transfer-moved-away-elsewhere = { $person } moved it to a workspace you're not in. What you see here is how it was when it left; it can't be edited here any more.

# Empty-workspace launchpad and the default command list
global-search-create-workspace = Create Workspace
global-search-rename-workspace = Rename Workspace
global-search-switch-workspace = Switch Workspace…
global-search-open-settings = Open Settings
global-search-open-file = Open File
search-action-workspace-create = Create a workspace
search-action-workspace-create-subtitle = Create and switch to a new empty workspace
search-action-workspace-create-default-name = Untitled
search-action-workspace-create-invalid-name = Enter a workspace name of 1 to 120 characters without control characters.
search-action-strip-create = Create a strip
search-action-strip-create-subtitle = Add a named strip to this workspace and go to it
search-action-strip-create-default-name = New strip
search-action-strip-rename = Rename this strip
search-action-strip-rename-subtitle = Give the focused strip a name
search-action-strip-invalid-name = Enter a strip name of 1 to 48 characters without control characters.
search-action-strip-rename-missing = There is no such strip in this workspace.
search-action-strip-move = Move a strip
search-action-strip-move-subtitle = Put a strip of this workspace higher or lower in its stack
search-action-strip-remove = Remove a strip
search-action-strip-remove-subtitle = Remove an empty strip from this workspace
search-action-strip-remove-not-empty = This strip still has windows. Move or close them first
search-action-strip-remove-last = A workspace keeps at least one strip

# Shortcut steps that ask before they run, and keyboard capture
shortcut-step-confirm-detail = “{ $shortcut }” wants to { $action } on { $resource }.
shortcut-step-type-phrase = Type “{ $phrase }” to confirm.
shortcut-step-confirm-title = { $action }?
shortcut-step-confirm = Allow
shortcut-step-ask-detail = “{ $shortcut }” asks for { $param }.
shortcut-step-continue = Continue
shortcut-step-yes = Yes
shortcut-step-no = No
shortcut-trigger-failed = The shortcut didn't run
osd-keys-captured = The app receives all keys. { $chord } restores shortcuts.
osd-keys-released = Shortcuts restored
osd-pad-mode = Tablet mode { $mode } of { $modes }
pad-map-hint = Shown while the button is held
pad-map-button = Button { $number }
pad-map-ring = Ring { $number }
pad-map-strip = Strip { $number }
pad-map-entry = { $control }: { $action }
pad-map-mode-switch = Next mode (now { $mode } of { $modes })
pad-map-shows-map = Shows this map
pad-map-app-decides = Up to the app
pad-map-nothing = Does nothing
pad-map-dial-scroll = Scroll
pad-map-dial-zoom = Zoom
pad-map-dial-brush-size = Brush size
pad-map-dial-volume = Volume
pad-map-dial-brightness = Brightness

# Peer views, opening documents, identity backup and invitations
peer-view-surface-title = Peer surface #{ $id }
peer-view-awaiting = Awaiting peer surface…
doc-open-no-handler = no app on this desktop opens .{ $ext } files ({ $name })
doc-open-no-file-type = { $name } has no file type to open it by
doc-open-no-workspace = there is no active workspace to open it in
identity-backup-busy = Finish the current identity flow before creating another backup.
identity-backup-locate-failed = Could not locate the active identity: { $error }
invite-workspace-verified-identity = Workspace-verified identity
palette-query-permission-roles = permission role

# Accessibility tree: the hardware-key readout
a11y-status-readout = Status readout

# Compositor notices: lock screen status, X11 compatibility, collaboration, nearby drops, captures
lock-screen-status = Lock status
xwayland-unavailable-title = X11 compatibility unavailable
xwayland-unavailable-body = Legacy X11 apps can’t open in this workspace right now. Use a Wayland app, or log out and back in.
collab-unavailable-services-root = The service directory could not be created: { $error }
collab-unavailable-socket-taken = Another session on this machine already owns the collaboration socket.
collab-unavailable-broker = The broker could not start: { $error }
fabric-focus-app-for-controller = Focus an app before forwarding a controller
fabric-focus-app-for-remote-input = Focus an app before using a remote input
launch-held-network-failed = the desktop's network could not start ({ $error })
nearby-drop-your-device = your device
nearby-drop-sending = Sending nearby
nearby-drop-sending-body = { $file } to { $device }
nearby-drop-sent = Sent nearby
nearby-drop-sent-body = { $file } to { $device }
nearby-drop-failed = Nearby drop failed
nearby-drop-failed-body = { $file } · { $device }
nearby-drop-received = Received nearby
nearby-drop-received-body = { $file } from { $device }
capture-screenshot-sent = Screenshot sent
capture-screenshot-not-sent = Screenshot not sent
capture-recording-audio-lost = Recording continues without audio
capture-recording-audio-lost-body = { $path } · { $reason }
capture-recent-audio-lost = Recent capture continues without audio
capture-failed = Capture failed
settings-not-saved-title = Setting not saved
settings-not-saved-body = A change to your settings couldn't be saved, so it went back to what it was. Check that the disk has free space, then try again.
idle-inhibit-unnamed-app = An app

# Sending a screenshot to a device or into the workspace files
screenshot-send-offered = Offered to { $device }; accept it there to save
screenshot-send-saved = Saved to workspace files as { $name }
screenshot-send-gone = The screenshot { $path } is gone
screenshot-send-device-offline = That device is no longer online
screenshot-send-create-failed = Cannot create { $path }: { $error }
screenshot-send-no-file-name = { $path } has no file name
screenshot-send-read-failed = Cannot read { $path }: { $error }
screenshot-send-save-failed = Cannot save to workspace files: { $error }
screenshot-send-too-many = The workspace Screenshots folder has too many files with this name
screenshot-send-sync-off = Device sync is not running
screenshot-send-no-folder = { $path } has no folder

# A window whose frames cannot be shown
foreign-window-unshowable = This window can't be shown.
foreign-window-not-responding = This app isn't responding.
foreign-window-close = Close
foreign-app-wait = Wait
foreign-app-close = Close app

# Why a speaker-group request failed, as Settings shows it
speaker-error-too-many-groups = too many speaker groups
speaker-error-group-exists = that speaker group already exists
speaker-error-group-gone = that speaker group no longer exists
speaker-error-not-in-group = that speaker is not in the group
speaker-error-calibration-range = those calibration values are out of range
speaker-error-request-expired = that speaker group request expired
speaker-error-group-changed = that speaker group changed; ask again from its device
speaker-error-too-many-memberships = this device is already in too many speaker groups
speaker-error-not-a-member = this device is not in that speaker group
speaker-error-too-many-sessions = too many speaker group sessions are active
speaker-error-group-unavailable = that speaker group is not available
speaker-error-none-ready = no speaker in that group is ready
speaker-error-session-gone = that speaker group session is no longer active
speaker-error-name = give the speakers a name of up to 64 characters
speaker-error-roles = each speaker needs its own output and its own role, and a stereo pair needs both sides
speaker-error-unavailable = speaker groups are unavailable
device-fabric-error-no-identity = this device has no personal identity yet
speaker-error-speaker-unavailable = that speaker is not available right now
speaker-error-output-disconnected = that output is no longer connected
speaker-error-already-playing = a speaker is already playing its sound
speaker-error-speaker-output-gone = that speaker's output is no longer connected
speaker-error-play-failed = could not play the sound: { $error }
speaker-error-device-invalid = that device id is not valid
speaker-error-output-invalid = that output is not valid
speaker-error-group-invalid = that speaker group is not valid
speaker-error-media-off = Turn on Media in Devices & Sync to use speakers across your devices
speaker-error-no-calibration = there is no finished calibration to apply
speaker-error-no-applied-calibration = there is no applied calibration to revert
speaker-error-calibration-unavailable = calibration is unavailable
speaker-error-calibration-gone = that calibration is no longer available
speaker-error-network-unavailable = the device network is unavailable
speaker-error-calibrate-on-leader = calibrate this group on the device that set it up
speaker-error-not-all-ready = every speaker must be ready before calibrating
speaker-error-request-failed = the speaker request failed
speaker-error-app-stopped = that application stopped playing
speaker-error-audio-unavailable = speaker group audio is unavailable
speaker-error-other-session = another speaker group session is already playing from this device

# Measuring speakers for calibration
speaker-calibration-reason-microphone-gone = that microphone is no longer connected
speaker-calibration-reason-output-gone = that speaker's output is no longer connected
speaker-calibration-reason-play-failed = could not play the sweep: { $error }
speaker-calibration-reason-clock = could not sync clocks with that speaker: { $reason }
clock-sync-failure-media-disabled = media continuity is off on this device
clock-sync-failure-no-reply = the other device did not answer clock sync
now-playing-speakers-playing = Playing on { $group }
now-playing-speakers-connecting = { $group }: connecting…
now-playing-speakers-syncing = { $group }: syncing clocks…
now-playing-speakers-clock-failed = { $group } can't play in time: { $reason }
speaker-calibration-reason-not-heard = no sweep stood out of the noise there
speaker-calibration-reason-not-allowed = that device has not allowed its microphone
speaker-calibration-reason-out-of-window = the request reached that device too late
speaker-calibration-reason-busy = that microphone was busy with another sweep
speaker-calibration-reason-capture-failed = the recording failed
speaker-calibration-reason-clock-unknown = that device's clock is not synchronised yet
speaker-calibration-reason-no-answer = that device did not answer in time
speaker-calibration-reason-not-at-seat = no microphone heard it clearly enough
speaker-calibration-failed = Could not measure { $speaker }: { $reason }
speaker-calibration-cancelled = Calibration was cancelled

# Wireless display projection
projection-service-unavailable-detail = Projection service is unavailable ({ $error }). Check the Tilo projector service.
projection-mode-unsupported = { $device } does not support { $mode } mode
projection-service-unavailable = Projection service is unavailable
projection-service-stopped = Projection service stopped

# Why an action on your devices failed, as its notification says
device-fabric-error-nearby-busy = Nearby Send staging is busy; retry when another transfer finishes
device-fabric-error-bt-identity-invalid = that Bluetooth accessory identity is invalid
device-fabric-error-bt-not-connected = only a paired accessory currently connected to this device can be handed off
device-fabric-error-bt-name-invalid = the local Bluetooth accessory name is invalid
device-fabric-error-bt-already-here = the Bluetooth accessory is already on this device
device-fabric-error-target-not-enrolled = the target is not an enrolled device
device-fabric-error-target-offline = the target device is offline
device-fabric-error-bt-coordinator = Bluetooth handoff coordinator is unavailable
device-fabric-error-bt-too-many = too many Bluetooth handoffs are already in progress
device-fabric-error-bt-approval-gone = that Bluetooth handoff approval is no longer pending
device-fabric-error-bt-inactive = that Bluetooth handoff is no longer active
device-fabric-error-bt-no-retry = that Bluetooth handoff cannot be retried
device-fabric-error-media-session-gone = that media session is no longer available
device-fabric-error-no-route = target has no current authenticated personal route
device-fabric-error-handoff-gone = that media handoff is no longer available
device-fabric-error-handoff-not-pending = that media handoff is no longer pending
device-fabric-error-handoff-no-retry = only a failed or cancelled media handoff can be retried
device-fabric-error-name-not-portable = file name is not portable
device-fabric-error-drop-one-item = nearby drop requires one regular file or folder
device-fabric-error-drop-empty = nearby drop cannot send an empty file
device-fabric-error-file-changed = file changed while being read
device-fabric-error-transfer-inactive = nearby transfer is not active
device-fabric-error-destination-unapproved = Nearby Send requires an approved destination
device-fabric-error-consent-expired = nearby transfer consent expired
device-fabric-error-destination-expired = Nearby Send destination expired
device-fabric-error-transfer-unrecoverable = nearby transfer is no longer recoverable
device-fabric-error-transfer-no-retry = nearby transfer is no longer retryable
device-fabric-nearby-device = Nearby device
device-fabric-error-output-gone = that audio output is no longer connected
device-fabric-error-input-gone = that input is no longer connected
device-fabric-error-audio-busy = audio changes are still being applied; try again in a moment
device-fabric-error-spatial-unavailable = spatial calibration is unavailable
device-fabric-error-proposal-stale = that calibration proposal is no longer current
device-fabric-error-no-applied-calibration = there is no applied calibration to revert
device-fabric-error-no-placement = this device has no published display placement
device-fabric-error-target-not-display = the target is not an online controllable display
device-fabric-error-target-no-display = the target has no published display
device-fabric-error-side-taken = another enrolled display already occupies that side
device-fabric-error-controller-not-enrolled = the controller is not an enrolled device
device-fabric-error-controller-no-input = the controller has no online pointer or keyboard capability
device-fabric-error-no-lease = that device does not have an active control lease
device-fabric-error-control-not-granted = that device has not granted control
device-fabric-error-lease-input = the active lease does not allow that input device
device-fabric-error-gamepad-capacity = typed-gamepad forwarding capacity is exhausted
device-fabric-error-gamepad-decided = typed-gamepad authorization is unknown or already decided
device-fabric-error-gamepad-no-lease = typed-gamepad target has not granted a lease
device-fabric-error-inspect-failed = source cannot be inspected: { $error }
device-fabric-error-transfer-start = nearby transfer could not start: { $error }
device-fabric-error-open-failed = file cannot be opened: { $error }
device-fabric-error-read-failed = file cannot be read: { $error }
continuity-disabled-clipboard = clipboard continuity is disabled on this device
continuity-disabled-notifications = notification continuity is disabled on this device
continuity-disabled-wifi-settings = Wi-Fi/settings continuity is disabled on this device
continuity-disabled-media = media continuity is disabled on this device
continuity-disabled-display-input = display/input continuity is disabled on this device
continuity-disabled-file-transfer = file-transfer continuity is disabled on this device
continuity-disabled-realtime = realtime-session continuity is disabled on this device
continuity-disabled-gamepad = typed-gamepad continuity is disabled on this device
continuity-disabled-app-continuations = app-continuation continuity is disabled on this device
remote-audio-all-system-audio = All system audio
remote-audio-error-target-not-output = the target is not an online audio output
remote-audio-error-unavailable = remote audio service is unavailable
remote-audio-error-too-many = too many remote audio routes are active
remote-audio-error-source-expired = that audio source picker entry expired
remote-audio-error-request-expired = that remote audio request expired
remote-audio-error-route-inactive = that remote audio route is no longer active
remote-audio-speaker-group = Speaker group
remote-audio-error-no-output = no local PipeWire output is available
remote-audio-error-stream-ended = that application audio stream ended
remote-capture-error-unavailable = remote capture service is unavailable
remote-capture-error-busy = remote capture service is busy
remote-capture-error-capacity = remote capture request capacity is exhausted
remote-capture-error-receiver-busy = remote capture receiver is busy
remote-capture-error-cannot-receive = this device cannot receive the requested remote input
remote-capture-error-unknown-request = unknown remote capture request
remote-capture-error-unknown-session = unknown remote capture session
tethering-error-continuity-off = Wi-Fi Continuity is disabled on this device
tethering-error-choose-phone = choose one of your phones that is online
tethering-error-unavailable = tethering coordinator is unavailable
tethering-error-too-many = too many tethering requests are already active
tethering-error-history-full = tethering history is temporarily full
tethering-error-session-inactive = that tethering session is no longer active
tethering-error-request-gone = that tethering request is no longer pending
gamepad-error-unavailable = physical gamepad manager is unavailable
gamepad-error-no-identity = this device's identity is unavailable
gamepad-error-target-unknown = that device isn't one of yours
gamepad-error-target-offline = target device is not an online remote device
gamepad-error-disconnected = that game controller is no longer connected
gamepad-error-already-forwarded = that game controller is already being forwarded
gamepad-error-busy = game controller input was busy while starting; try again
gamepad-error-unknown = physical gamepad forwarding is unknown
gamepad-error-open-failed = could not open game controller: { $error }
gamepad-error-prime-failed = could not prime game controller: { $error }

# Why a Wi-Fi network could not be shared with your devices
wifi-share-error-enterprise = enterprise Wi-Fi requires identity, EAP method, and certificate setup on this device
wifi-share-error-no-passphrase = this Wi-Fi security mode has no portable passphrase
wifi-share-error-unknown-security = unrecognized Wi-Fi security cannot be shared safely
wifi-share-error-unvalidated = Wi-Fi security could not be validated for sharing
wifi-share-error-no-presence = this device has not published fabric presence yet

# Why installing an app from the catalogue failed
catalogue-error-missing = this system has no app catalogue (app is missing)
catalogue-error-spawn = could not start loading it
catalogue-error-run = could not run the app catalogue: { $error }
catalogue-error-exited = the app catalogue exited { $status }
catalogue-error-flathub-run = could not run Flatpak: { $error }
catalogue-error-flathub-invalid = this isn't a Flathub app
catalogue-error-flathub-untrusted = your Flathub source must be enabled, check signatures and use the official repository
catalogue-error-flathub-missing = Flathub isn't set up on this device
catalogue-error-flathub-not-found = it was installed but can't be found to open

# Why sending to a nearby device failed
nearby-error-destination-not-folder = Nearby Send destination is not a directory
nearby-error-too-large = nearby transfer exceeds the size limit
nearby-error-destination-name = Nearby Send destination name is unavailable
nearby-error-outside-grant = Nearby Send source is outside this Files grant
nearby-error-one-item = Nearby Send requires one regular file or folder
nearby-error-queue-full = nearby transfer queue is full
nearby-error-quota-full = nearby transfer quota is full
nearby-error-disk-full = not enough free disk space for this transfer
nearby-error-too-many = too many nearby transfers
nearby-error-not-regular = source is not a regular file
nearby-error-folder-too-many = folder contains too many entries
nearby-error-folder-special = folder contains a link or special file
nearby-error-folder-too-large = folder exceeds the size limit
nearby-error-source-changed = nearby transfer source changed while staging
nearby-error-name-not-portable = source name is not portable

# AirDrop lab receiver verification
airdrop-verify-title = Verify AirDrop receiver
airdrop-verify-send-once = Send once
airdrop-verify-detail = File: { $name }
    Size: { $bytes } bytes
    Receiver: { $endpoint }
    Enter the receiver's SHA-256 certificate fingerprint as 64 hexadecimal digits from its trusted console or another independently verified channel. This sends the staged file once, only if the fingerprint matches. Cancel if you cannot verify the receiver.
airdrop-error-unavailable = AirDrop lab requires an unlocked local session, file-transfer permission and current Files access.
airdrop-error-source = AirDrop lab accepts one readable regular file up to 16 MiB.
airdrop-error-busy = Another AirDrop attempt is still running. Cancel it before starting another.
airdrop-error-failed = AirDrop transfer failed. Verify the receiver again before a new attempt.
airdrop-error-declined = AirDrop transfer was cancelled before sending.
airdrop-error-verification = Fingerprint differs. Check the receiver; nothing sent.
airdrop-error-fresh-consent = Start a new AirDrop attempt to verify the receiver and approve this file again.

# A document a tab could not restore
hosted-document-unavailable = { $path } is unavailable: { $error }

# A camera, microphone or scanner shared from another of your devices
remote-capture-source-label = { $app } on another device

# Print jobs: the title a print queue shows when the app gave none
print-untitled-document = Document

a11y-speaker-listen = Let { $leader } use this microphone to calibrate { $group }
a11y-speaker-listen-description = It records only while each speaker plays a test sweep; the recording stays on this device and only timing and level are sent
a11y-speaker-listen-allow = Allow this microphone
a11y-speaker-listen-decline = Do not allow this microphone
a11y-speaker-listener = { $group } can calibrate with this microphone
a11y-speaker-listener-description = Speaker calibration may listen with this microphone; activate to stop
speaker-calibration-listening-refused = { $microphone } was not allowed to listen, so nothing can be measured from where you sit
speaker-error-listening-unavailable = choose a microphone that can listen where you sit
lock-screen-unlock = Unlock session
shortcut-name-dictation-toggle = Start or finish dictation
shortcut-name-dictation-finish = Finish dictation
shortcut-name-dictation-cancel = Cancel dictation
dictation-title = Dictation
dictation-consent = Record your microphone for this dictation? Audio stays on this device.
dictation-preparing = Checking the focused editable field…
dictation-loading = Loading local recognition. The microphone is off.
dictation-listening = Microphone on. Speak, then finish to insert.
dictation-finishing = Microphone off. Finishing your text…
dictation-stopping = Stopping dictation…
dictation-editable-required = Focus an editable field. Passwords cannot receive dictation.
dictation-unsupported-field = Dictation is not supported in terminal fields yet. Focus another editable field.
dictation-busy = The previous dictation is still stopping. Try again shortly.
dictation-target-changed = The field changed. Dictation was canceled without inserting text.
dictation-timeout = Dictation timed out. No text was inserted.
dictation-unavailable = Local dictation is unavailable. No text was inserted.
dictation-model-required = Install an approved local speech model before using dictation.
dictation-language-unavailable = This speech model does not support the requested language.
dictation-microphone-unavailable = Microphone access is unavailable. Check your input device.
dictation-vocabulary-unavailable = Your personal dictionary changed or became unavailable. Dictation stopped; start a new session.
dictation-audio-interrupted = Microphone audio was interrupted. No text was inserted.
dictation-live-preview = Preview: { $text }
dictation-open-settings = Set up
dictation-edit-rejected = Microphone off · The original field refused the text or changed. Your transcript is kept privately for 10 minutes. Copy exports it to your clipboard; discard removes it.
dictation-delivery-unknown = Microphone off · Text insertion was not confirmed. Check the original field before inserting again. Your transcript is kept privately for 10 minutes; copy exports it to your clipboard.
dictation-action-start-detail = Request one-session microphone consent for the focused field, or finish the current recording.
dictation-action-finish-detail = Stop the microphone and insert the final corrected text into its original field.
dictation-action-cancel-detail = Stop dictation and discard its private draft.
dictation-stopping-microphone = Stopping microphone…

search-action-axis-enable = Enable experimental axis navigation
search-action-axis-enable-subtitle = This session: step between groups and gather focus into view
search-action-axis-disable = Disable experimental axis navigation
search-action-axis-disable-subtitle = Restore ordinary window navigation for this session
search-action-axis-gather = Gather the focused window into view
search-action-axis-gather-subtitle = Experimental axis navigation: reveal focus without animation
axis-navigation-disabled = Experimental axis navigation is disabled

search-provider-device-shell = Device shell
terminal-request-device-shell = Request this device's own shell
terminal-device-shell-owner-required = Only the device owner can request its own shell

terminal-inspect-device-shell = Inspect this device's shell session
terminal-run-device-shell = Run a command in this device's shell
terminal-read-device-shell = Read this device's shell output

terminal-device-shell-session-param = Exact local session UUID from Inspect
terminal-device-shell-command-param = One reviewed command in this device shell

terminal-device-shell-confirm-param = Confirm this reviewed device-shell operation

shortcut-name-tab-menu = Show tab menu
search-action-tab-menu = Show tab menu
search-action-tab-menu-subtitle = Actions for the focused tab
search-action-tab-menu-unavailable = No focused tab is available

dictation-transcript-retained = Microphone off · Text is too long to insert in one step. Kept privately for 10 minutes. Copy exports all text to your clipboard; discard removes it.
dictation-transcript-copied = Complete transcript copied. Paste it into your chosen field. The private draft remains until you discard it or 10 minutes expire.
dictation-copy-failed = Clipboard unavailable. Your complete transcript is still kept privately; retry copying or discard it.
search-action-tab-drag-cancel = Cancel tab drag
search-action-tab-drag-cancel-subtitle = Keep the dragged tab in its original place
search-action-tab-drag-cancel-unavailable = Tab dragging cannot be cancelled while the session is locked.

display-confirm-answer-pending = Saving the display answer…
display-confirm-answer-failed = Could not save the display answer: { $reason }

display-sheet-saving-layout = Saving display layout…

session-diagnostics-title = Session timing diagnostics
session-diagnostics-configure = Configure timing for subsequent app launches
session-diagnostics-inspect = Inspect session timing diagnostics
session-diagnostics-owner-required = Session diagnostics require the authenticated owner's admin control socket while unlocked.
session-diagnostics-app-filter = Only return launch attempts for this app ID
session-diagnostics-presentation = Record content-free input and presentation timing
session-diagnostics-gpu_stages = Record GPU stage timing
session-diagnostics-gpu_timestamps = Enable GPU timestamps
session-diagnostics-gpu_dispatch_timestamps = Enable GPU dispatch timestamps
session-diagnostics-dispatch-requires-timestamps = GPU dispatch timing requires GPU timestamps.
session-diagnostics-profile-start = Record a desktop performance profile
session-diagnostics-profile-stop = Stop recording the performance profile
session-diagnostics-profile-seconds = How many seconds to record (default 30)
session-diagnostics-profile-seconds-range = A profile records between 1 and { $max } seconds.
session-diagnostics-profile-no-state-dir = There is no state directory to save the profile in; set HOME or XDG_STATE_HOME.
session-diagnostics-profile-already-recording = A profile is already recording to { $path }. Stop it first.
session-diagnostics-profile-not-created = The profile could not be created: { $error }

device-fabric-error-layout-changed = Display layout changed or unavailable. Refresh and try again.
search-action-activity-acknowledge = Mark activity as seen
search-action-activity-acknowledge-subtitle = What you’ve looked at stops counting as new
search-action-activity-acknowledge-keywords = catch up read seen review acknowledgment
search-action-activity-tasks = Review workspace goals and agent work
search-action-catch-up-tasks-subtitle = Inspect waiting tasks, results and action history
search-action-activity-tasks-keywords = activity catch up goals attention decision waiting agent tasks
host-history-task-refused-title = Task could not be changed
host-history-task-refused-body = The workspace did not accept this change. Any draft is kept; check your identity and workspace permissions before retrying.

task-review-inspect = Inspect task evidence
task-review-steer = Change task instructions
task-review-cancel = Cancel task run
task-review-retry = Retry task
task-review-status = Inspect task review availability
task-review-provider = Task review

dictation-vocabulary-changed = Dictation stopped because your personal dictionary changed. Start a new session to use the updated words.
dictation-vocabulary-draft-retained = Your personal dictionary changed. Dictation has stopped. You can copy the latest draft; it may be incomplete.

shell-launch-registry-unavailable = The app process registry is unavailable.
shell-launch-writer-still-closing = The previous app is still closing. Try opening it again after it finishes.

shell-window-restore-interrupted = This saved window did not finish reopening.
shell-window-retry = Reopen
shell-window-app-missing = { $app } isn't on this computer
shell-window-get-app = Get { $app }
shell-window-close = Close
task-review-approve = Approve task run

# Shared activity label used by native task-review rendering.
activity-task-approve-run = Approve run

device-fabric-error-input-operation-unavailable = Input control is busy. Try again.
device-fabric-error-input-decline-unavailable = Your declined input devices could not be read.
device-fabric-error-input-decline-save-failed = Input control was revoked, but the decline could not be saved. Automatic input remains disabled.

device-fabric-error-input-sequence-exhausted = Input sequencing is exhausted. Request a new control lease.

device-fabric-error-input-cleanup-pending = Previous input is still being released. Try again.

search-provider-system-settings = System settings
setting-never = Never
setting-minutes = { $minutes ->
        [one] 1 minute
       *[other] { $minutes } minutes
    }
setting-on = On
setting-off = Off
setting-mode-dark = Dark
setting-mode-light = Light
setting-mode-automatic = Automatic
setting-mode-unset = Follows the theme
setting-get = { $name }: { $value }
setting-get-detail = Read the current value and the values that are allowed
setting-power-lock-after = Lock screen after
setting-power-lock-after-words = lock screen automatically sperren sperrzeit
setting-power-blank-after = Turn screen off after
setting-power-blank-after-words = screen off blank display dark bildschirm ausschalten abschalten dunkel
setting-power-suspend-after = Suspend after
setting-power-suspend-after-words = sleep standby energy saving bereitschaft ruhezustand energiesparen
setting-power-auto-suspend = Suspend when idle
setting-power-auto-suspend-words = automatic sleep standby inactivity automatisch inaktiv
setting-display-scale = Display scale
setting-display-scale-words = scaling zoom size bildschirm skalierung groesse
setting-display-refresh-rate = Display refresh rate
setting-display-refresh-rate-words = hertz frequency frames bildwiederholrate bildfrequenz
setting-keyboard-layout = Keyboard layout
setting-keyboard-layout-words = language input source keymap tastatur sprache belegung
setting-appearance-mode = Appearance mode
setting-appearance-mode-words = dark light night theme design dunkel hell nacht
display-remote-heading = Your other screens
display-remote-title = Display { $number }: { $name } on { $device }
display-remote-row = { $status } · { $title }
display-remote-row-description = Include this screen in your shared display layout
display-remote-offline = Offline
display-remote-offline-excluded = Offline, not in the layout
display-remote-switched-off = Switched off on its device
display-remote-in-layout = In layout
display-remote-out-of-layout = Not in layout
display-remote-unavailable = This shared display is no longer available.
display-remote-permission = Your workspace role does not allow changing the shared display layout.
display-remote-stale = This display arrangement changed. Refresh before editing.
search-action-shared-display-inspect = Inspect shared displays
search-action-shared-display-inspect-subtitle = Certified outputs, display numbers, inclusion and current arrangement revisions
search-action-shared-display-include = Set shared display inclusion
search-action-shared-display-include-subtitle = Include or exclude one certified output without granting input access
activity-morning-choose = Choose for morning
activity-morning-remove = Remove morning source
activity-morning-empty = Morning sources: choose a resource in Explore, then Details. Up to eight sources; none connect automatically.
activity-morning-description = Chosen morning sources: provider previews from authorized workspace resources. Refresh or draft a schedule explicitly.
activity-morning-unavailable = This source isn't available here
activity-morning-source-unnamed = Chosen source
activity-morning-source = { $source }, last activity { $time } · may be out of date
activity-morning-open = Open morning source

# Canonical first-use action admission; never includes private identity material.
onboarding-semantic-unavailable = This setup action is no longer available at this step.
onboarding-semantic-failed = Setup could not complete this action. The desktop remains protected.
activity-morning-payload-unavailable = Provider summary unavailable. Open the source to inspect or refresh it.

activity-morning-refresh = Refresh source
activity-morning-draft = Draft scheduled refresh…
activity-morning-draft-description = Opens a draft in Shortcuts. Choose a schedule and review its permissions before saving; nothing is armed yet.
activity-morning-command-unavailable = Source command unavailable
activity-morning-command-pending = Request pending
activity-morning-command-accepted = Request accepted; inspect its result in Actions
activity-morning-command-refused = Request refused
activity-morning-recorded = { $source } · last recorded workspace activity: { $time }

activity-morning-next = Next chosen source

# Installed prepared setup and original-file migration.
onboarding-prepared-title = Welcome home

onboarding-prepared-body = Enter the password you chose while preparing Tilo to bring your identity and accounts here.

onboarding-prepared-password = Your password
onboarding-prepared-peer-waiting = Waiting for your other computer to offer the transfer.
onboarding-prepared-peer-code = Your other computer shows { $code }. Check that both screens show the same code, then approve this computer there.

onboarding-prepared-working = Bringing your setup here…

onboarding-prepared-failed = We couldn’t bring your prepared setup here. Try again.
onboarding-prepared-other-person = This computer already has another person’s identity, so the prepared setup was left untouched. Restore that person’s own backup or connect their account instead.

onboarding-migration-title = Bring your things into Tilo

onboarding-migration-body = Bring over the documents and apps you chose during setup. Your originals stay where they are.

onboarding-migration-reviewed = Your migration is ready. Copy your things into a separate import first.

onboarding-migration-adopt-body = Your import is verified. Add it to your home; existing files and your Tilo identity stay in place.

onboarding-migration-working = Bringing your things over…

onboarding-migration-complete = Your things are ready in Tilo.

onboarding-migration-incomplete = Some things still need attention. Existing files were kept; the imported copies remain separately available.

onboarding-migration-failed = Your migration could not continue. Your originals and retained import have been kept.

onboarding-migration-cancelled = Migration stopped. Your imported files are kept so you can continue later.

onboarding-migration-review = Review my migration

onboarding-migration-start = Copy my things

onboarding-migration-adopt = Add to my home

onboarding-migration-retry = Retry

onboarding-migration-cancel = Stop

onboarding-migration-later = Bring my things later

onboarding-migration-progress = { $files } files copied

onboarding-portable-home-title = Bring your files into Tilo

onboarding-portable-home-body = Enter the password you chose during setup to review your files before adding them to this computer.

onboarding-portable-home-unavailable = Your transfer is not available on this computer yet. Try again or continue without bringing your files over.

onboarding-migration-resume-busy = Finish the current setup before opening your file migration.

onboarding-migration-resume-unavailable = There is no deferred file migration available for your current identity.

onboarding-existing-files-available = Your existing files are available in Tilo. Changes you make here also appear in your previous system.

onboarding-existing-files-prepare-apps = Prepare my apps

onboarding-existing-files-use = Use my existing files

onboarding-existing-files-body = Use your existing files directly. Changes you make in Tilo also appear in your previous system.

onboarding-existing-files-apps-reviewed = Prepare your selected apps and their data for Tilo. Your existing apps stay in your previous system.

onboarding-existing-files-apps-adopt = Your apps are prepared. Add them and their data to Tilo.

onboarding-portable-applications-title = Bring your apps with you

onboarding-portable-applications-body = Restore the apps you selected and their data from your installer. Your original apps and files stay unchanged.

onboarding-portable-applications-start = Restore my apps

onboarding-portable-applications-unavailable = Your apps could not be restored. You can retry without replacing your existing files.

onboarding-browser-title = Bring your browser with you

onboarding-browser-body = Your selected bookmarks and history are ready to bring into Tilo.

onboarding-browser-start = Use my browser data

onboarding-browser-unavailable = Your browser data is still waiting. Try again or continue for now.

onboarding-backup-unavailable = A personal identity backup can’t be created on this device.

onboarding-ready-welcome = Welcome to Tilo

onboarding-ready-next = Make yourself at home, or take a quick tour first.

onboarding-migration-cleanup-pending = Your things are ready in Tilo. Some temporary files still need to be removed.

onboarding-migration-cleanup-unavailable = Your things are ready in Tilo. Temporary file cleanup could not be confirmed.

onboarding-migration-cleanup-resume = Retry cleanup

onboarding-migration-cleanup-running = Your things are ready in Tilo. Removing temporary files…

# Why the first-use copy stopped, reported with its status.
onboarding-copy-cleanup-unavailable = Temporary file cleanup is unavailable.
onboarding-copy-cleanup-unconfirmed = Final cleanup could not be confirmed.
onboarding-copy-cleanup-mismatch = The cleanup confirmation belongs to a different copy.
onboarding-copy-receipt-mismatch = The copy confirmation belongs to a different copy.
onboarding-copy-cleanup-confirmation-unavailable = Cleanup confirmation is unavailable.

session-retained-retirement = Retained file cleanup
session-retained-retirement-detail = Inspect this session’s retained file owners and observed cleanup.

search-provider-authentication-prompts = Administrator authentication
portal-auth-identity-param = Administrator account
portal-auth-request-expired = This authentication request is no longer available.
portal-auth-local-admin-required = Administrator authentication requires the unlocked device owner's session.
qs-network-details = Wi-Fi networks
qs-bluetooth-details = Bluetooth devices
search-action-quick-settings-network-subtitle = Browse available Wi-Fi networks
search-action-quick-settings-bluetooth-subtitle = Browse paired Bluetooth devices

search-provider-file-picker = File chooser
file-picker-inspect = Inspect file chooser
file-picker-select = Select a listed file
file-picker-navigate = Open a listed folder
file-picker-up = Go to the parent folder
file-picker-confirm = Confirm file selection
file-picker-cancel = Cancel file chooser
file-picker-filename = Set the save filename
file-picker-replace = Replace the existing file
file-picker-keep-existing = Keep the existing file
file-picker-next-filter = Change the file filter
file-picker-page-param = Catalogue page, starting at zero.
file-picker-state-param = Exact state token returned by the current chooser inspection.
file-picker-entry-param = Exact entry identity returned by the current chooser inspection.
file-picker-filename-param = One filename within the current permitted folder.
file-picker-confirm-param = Confirm this file handoff or replacement.
file-picker-expired = The file chooser or its selection changed. Inspect it again.
file-picker-busy = The file chooser has too many pending operations.
file-picker-entry-unavailable = This entry is not in the current file chooser.
file-picker-page-unavailable = This file chooser page is unavailable.
file-picker-owner-required = Only the current workspace owner can operate this file chooser.
file-picker-outside-scope = This folder is outside the file chooser's permitted roots.
file-picker-operation-unavailable = This operation is unavailable in the current file chooser state.
file-picker-declared-operation = Use a declared file chooser operation.

portal-chooser-file-grant-unavailable = The selected file is no longer available. Choose it again.

## The agent composer: one field to ask an agent or tell it what to do
agent-quick-title = { $agent }
agent-quick-about-selection = About your selection in { $app }: “{ $selection }”
agent-quick-about-window = About { $app }; nothing is selected
agent-quick-accessible-name = Agent composer
agent-quick-shots-context = About your selection in Notes: “[tilo OS](search for the tilo homepage)”
agent-quick-shots-draft = What does this link point to?
agent-quick-shots-question = Does this link point anywhere yet?
agent-quick-shots-answer = Not yet. It's written as a link, but the part in parentheses is a note to look up the address rather than an address itself.
agent-quick-shots-request = Replace the parenthesis with the real link
agent-quick-shots-task = Link the tilo homepage
agent-quick-shots-steer = Use the https address
agent-quick-placeholder-idle = Ask { $agent }, or say what to do
agent-quick-placeholder-running = Tell { $agent } how to continue
agent-quick-editor-name = Your message to the agent
agent-quick-send = Send
agent-quick-stop = Stop
agent-quick-close = Close
agent-quick-insert = Insert
agent-quick-save-note = Save as note
agent-quick-you-asked = You: { $text }
agent-quick-thinking = Thinking…
agent-quick-steering = Sending…
agent-quick-stopping = Stopping…
agent-quick-steer-saved = Sent. { $agent } will take it into account.
agent-quick-steer-applied = { $agent } is following your note.
agent-quick-run-starting = Starting: { $task }
agent-quick-run-working = Working on it: { $task }
agent-quick-run-waiting = Waiting for your confirmation
agent-quick-draft-confirm = Allow
agent-quick-draft-edit = Edit
agent-quick-draft-decline = Decline
agent-quick-run-done = { $agent } is done.
agent-quick-run-attention = { $agent } stopped and needs you.
agent-quick-run-stopped = Stopped.
agent-quick-outcome-unknown = No reply yet. Your message is still here.
agent-quick-refused = { $agent } couldn't take that. Your message is still here.
agent-quick-stop-refused = Couldn't stop it. Try again.
## Search vocabulary: the words people use for actions and settings pages.
## Each `search-kw-*` message is a comma-separated list; desktop search matches
## the lists of every language whatever language the desktop speaks.
search-kind-settings-pages = Settings
search-settings-page-subtitle = Settings
search-toggle-wifi = Wi-Fi
search-toggle-wifi-subtitle = Turn Wi-Fi on or off
search-toggle-bluetooth = Bluetooth
search-toggle-bluetooth-subtitle = Turn Bluetooth on or off
search-toggle-airplane-mode = Airplane mode
search-toggle-airplane-mode-subtitle = Turn every wireless radio off
search-badge-state-current = Current
search-provider-settings-pages = Settings pages
search-kw-dictation-toggle = dictate, dictation, speech to text, voice typing, voice input, microphone
search-kw-dictation-finish = stop dictation, end dictation, finish typing
search-kw-dictation-cancel = abort dictation, discard dictation
search-kw-navigation-axis-enable = experimental, navigation, axis, arrow navigation
search-kw-navigation-axis-disable = experimental, navigation, axis, arrow navigation
search-kw-navigation-axis-gather = find window, bring window back, lost window, center window, scroll to window
search-kw-overview-toggle = overview, workspaces, mission control, expose, activities, show all workspaces
search-kw-settings-open = workspace settings, room settings, workspace options, preferences
search-kw-settings-members-open = people, members, collaborators, users, who is here, permissions, roles
search-kw-settings-privacy-open = privacy, security, permissions, access control
search-kw-settings-apps-open = apps, files, workspace apps, default apps
search-kw-identity-connect = identity, account, login, sign in, add account
search-kw-identity-select = identity, account, switch account, use account
search-kw-identity-devices-copy = devices, copy list, device list
search-kw-identity-device-enroll = add device, pair device, new device, link device, enroll
search-kw-identity-home-peer-enroll = home server, nas, pair server, home peer
search-kw-identity-device-pair-discover = pair device, nearby device, find device, add device, home server
search-kw-identity-device-pair-approve = approve device, pair device, verification code
search-kw-identity-device-pair-reject = reject device, cancel pairing, deny device
search-kw-identity-device-revoke = remove device, revoke device, forget device, unlink
search-kw-quick-settings-open = quick settings, control center, control centre, system menu, toggles, panel
search-kw-quick-settings-clipboard = clipboard, copy history, paste history, clipboard manager, copied
search-kw-quick-settings-devices = devices, media, cast, airplay, audio devices, output, speakers
search-kw-projection-open = screen mirroring, cast, projector, external display, second screen, extend, mirror, present, hdmi, beamer
search-kw-capture-open = screenshot, screen capture, snip, snipping tool, screen grab, record screen, print screen, picture in picture, pip, screencast, screenshot tool
search-kw-capture-screenshot-send = send screenshot, share screenshot, last screenshot, screenshot
search-kw-collaborators-open = collaborators, people, who is here, participants, presence
search-kw-presence-follow = follow, spectate, watch someone, follow person
search-kw-presence-follow-stop = unfollow, stop following
search-kw-presence-status-set = status, away, busy, availability
search-kw-presence-status-sharing-set = status privacy, share status, hide status
search-kw-presence-wave = wave, say hi, hello
search-kw-presence-availability-set = status, available, focusing, away, busy, do not disturb
search-kw-presence-visibility-set = privacy, invisible, hide, what people see, show where I am
search-kw-room-activity-open = activity, history, what changed, changes, timeline, catch up, log, recent
search-kw-peer-invite = invite, share, add person, collaborate, join link, send invite
search-kw-peer-connect = join, join workspace, accept invite, connect, enter code, public workspaces, browse workspaces, discover, directory, community
search-kw-workspace-publish = public, publish, list, directory, default role, discoverable, open to everyone
search-kw-workspace-unpublish = private, unpublish, unlist, hide from directory
search-kw-workspace-report = report, abuse, spam, flag
search-kw-peer-invite-named = invite by name, invite
search-kw-agent-invite = ai, assistant, claude, agent, bot, helper
search-kw-tab-new-terminal-here = terminal, shell, console, command line, new tab
search-kw-window-new-terminal = terminal, shell, console, command line, new window
search-kw-tab-duplicate = duplicate, clone, copy tab
search-kw-tab-open-beside = open app, new tab, launch, start app
search-kw-tab-menu = tab menu, context menu, right click
search-kw-tab-drag-cancel = cancel drag, abort drag
search-kw-tab-close = close, close tab, close window, quit, exit, kill
search-kw-tab-reopen = reopen, undo close, restore tab, closed tab, bring back
search-kw-tab-split-right = split, side by side, tile, split view, split screen
search-kw-window-focus-previous = previous window, switch window, alt tab, back
search-kw-window-focus-next = next window, switch window, alt tab
search-kw-strip-focus-previous = up, strip above, row above, previous strip, switch strip
search-kw-strip-focus-next = down, strip below, row below, next strip, switch strip
search-kw-workspace-previous = previous workspace, switch workspace, back, desktop
search-kw-workspace-next = next workspace, switch workspace, desktop
search-kw-strip-create = new strip, add strip, add row, new lane, create strip
search-kw-strip-rename = rename strip, name strip, strip name, label row
search-kw-strip-move = move strip, reorder strips, strip up, strip down, rearrange rows
search-kw-tab-previous = previous tab, left tab, back
search-kw-tab-next = next tab, right tab, forward
search-kw-group-move-left = move window, arrange, reorder, left
search-kw-group-move-right = move window, arrange, reorder, right
search-kw-group-move-to-strip-previous = move window up, send to row above
search-kw-group-move-to-strip-next = move window down, send to row below
search-kw-group-move-to-display-previous = move to monitor, other screen, send to display, previous monitor
search-kw-group-move-to-display-next = move to monitor, other screen, send to display, next monitor
search-kw-group-maximize = maximize, restore, fill screen, zoom window
search-kw-group-fullscreen = fullscreen, full screen, immersive
search-kw-group-narrower = narrower, shrink, smaller, resize
search-kw-group-wider = wider, grow, bigger, larger, resize
search-kw-tab-move-to-group-previous = merge tab, move tab, tab left
search-kw-tab-move-to-group-next = merge tab, move tab, tab right
search-kw-window-focus-nth = focus window number, go to window, jump to window
search-kw-strip-focus-nth = strip number, go to strip, jump to strip, switch strip
search-kw-group-restore = restore, unmaximize, un-maximize, normal size, shrink window
search-kw-tab-move-to-new-group = new group, detach, pop out, separate tab
search-kw-notifications-dnd-toggle = do not disturb, dnd, silence, mute notifications, focus mode, quiet, notifications off
search-kw-appearance-theme-dark = dark mode, dark theme, night mode, dark, black theme, night
search-kw-appearance-theme-light = light mode, light theme, day mode, bright, light, white theme
search-kw-appearance-high-contrast = high contrast, contrast, accessibility, visibility
search-kw-appearance-language = language, locale, translate, german, english, deutsch, switch language
search-kw-record-bring = add item, import, bring, add document, attach
search-kw-shortcut-help-toggle = help, shortcuts, keyboard shortcuts, cheat sheet, hotkeys, keys, guide
search-kw-desktop-tour-open = tour, tutorial, intro, onboarding, getting started, guide, welcome, walkthrough
search-kw-quick-settings-brightness-set = brightness, screen brightness, backlight, display brightness
search-kw-quick-settings-volume-set = volume, sound, loudness, audio level, speaker
search-kw-quick-settings-power-profile-set = power profile, battery saver, performance mode, power mode, power saving, balanced, energy
search-kw-quick-settings-vpn-set-enabled = vpn, tunnel, wireguard, proxy
search-kw-quick-settings-wifi-toggle = wifi, wi-fi, wlan, wireless, internet, network, wireless network
search-kw-quick-settings-bluetooth-toggle = bluetooth, headphones, pair, wireless
search-kw-quick-settings-airplane-mode-toggle = airplane mode, flight mode, aeroplane, radios off, offline
search-kw-session-lock = lock screen, lock, lock computer, secure, screensaver, away
search-kw-emoji-picker = emoji, emojis, smiley, emoticon, :
search-kw-session-switch-user = switch user, switch account, change user, other user, fast user switching
search-kw-session-suspend = sleep, suspend, standby, hibernate, nap
search-kw-session-restart = restart, reboot, reset computer, power cycle
search-kw-session-shut-down = shut down, shutdown, power off, turn off, switch off, halt
search-kw-session-logout = log out, logout, sign out, sign off, end session, leave

storage-app-name = Storage
storage-volume-home = your files
storage-volume-system = the system
storage-low-title = Storage is almost full
storage-low-body = { $free } left for { $volume }. Delete large downloads or open Settings, Storage to free space.
storage-full-title = Storage is full
storage-full-body = Only { $free } left for { $volume }. New files and changes may not be saved until you free space. Everything else keeps working.

## Desk widgets

desk-team-title = In this workspace
# Where a person is: their strip, then the window they are on.
desk-team-place = { $strip } · { $window }
desk-team-more = +{ $count } more

drives-app-name = Drives
drives-gone = This drive is no longer connected.
drives-unavailable = Drives can't be opened right now.
drives-no-files = Files is not installed, so the drive can't be shown.
drives-no-workspace = Open a workspace to see the drive.
drives-unnamed-internal = Drive in this computer
drives-unnamed-external = External drive
drives-consent-title = Open { $drive }?
drives-consent-title-unnamed-internal = Open this drive in your computer?
drives-consent-title-unnamed-external = Open this external drive?
drives-consent-internal = Inside this computer · { $size }
drives-consent-external = External drive · { $size }
drives-consent-detail = Choose how to open this drive.
drives-consent-detail-known = You opened this drive safely before. Choose how to open it this time.
drives-consent-open-safely = Open safely: for drives you don't know. A bit slower.
drives-consent-open = Open: fastest. For your own drives.
drives-consent-remember = Remember this drive and don't ask again
drives-consent-remember-safely = Remember this drive and suggest opening it safely
drives-consent-confirm = Open
drives-consent-not-now = Not now
drives-read-only-title = { $drive } opened read-only
drives-read-only-hibernated = Windows is hibernated or uses Fast Startup on this drive. Shut Windows down fully to make changes.
drives-read-only-not-shut-down = Windows wasn't shut down properly. Shut Windows down fully to make changes.
drives-read-only-check = Windows wants to check this drive for errors. Shut Windows down fully to make changes.
drives-failed-title = { $drive } couldn't be opened

# The top bar's workspace menu and profile card
workspace-menu-settings = Workspace settings…
workspace-menu-new = New workspace
workspace-menu-join = Join a workspace…
workspace-menu-report = Report workspace…
workspace-menu-leave = Leave workspace
profile-action-identities = Your identities
profile-action-backup = Back up your identity…
profile-action-pair-device = Pair a device…
# Strip switcher: the strip ribbon grown into a list of strips and workspaces.
strip-switcher-a11y = Strips
strip-switcher-unnamed = Strip { $number }
strip-switcher-new-strip = New strip
strip-switcher-rename-named = Rename { $name }
strip-switcher-name-placeholder = Strip name
strip-switcher-refused = Couldn't change the strip



## Pairing a new device of the person.
pairing-title = Pair a new device
pairing-making = Making a link…
pairing-waiting = Waiting for your new device…
pairing-copy-link = Copy link
pairing-done-button = Done
pairing-copied = Copied
pairing-way-phone = Phone
pairing-way-phone-how = Scan with the camera
pairing-way-computer = Tilo computer
pairing-way-computer-how = Paste the link into Join a workspace…
pairing-way-browser = Browser
pairing-way-browser-how = Open the link
pairing-safe = This computer asks you first and shows a matching code. The link works once, for 10 minutes.
pairing-qr-alt = Pairing link for your new device
pairing-done = { $device } is now one of your devices.
pairing-asks-title = A device asks to be you
pairing-asks-body = { $device }{ $from }, { $when }. Approving lets it act as you. Only approve if it shows { $code }.
pairing-asks-from = {" "}({ $country })
pairing-approve = Approve { $code }
pairing-decline = Decline
pairing-answer-failed = The device was not answered: { $error }

search-step-volume-up = Louder
search-step-volume-down = Quieter
search-step-brightness-up = Brighter
search-step-brightness-down = Dimmer
search-step-volume-mute = Mute or unmute
search-level-percent = { $percent } %
search-kw-quick-settings-volume-up = louder, volume up, turn up, lauter, lauter machen
search-kw-quick-settings-volume-down = quieter, softer, volume down, turn down, leiser, leiser machen
search-kw-quick-settings-volume-mute = mute, unmute, silence, stumm, stummschalten, ton aus
search-kw-quick-settings-brightness-up = brighter, brightness up, heller
search-kw-quick-settings-brightness-down = dimmer, darker, brightness down, dunkler
desk-monitor-title = This machine
desk-monitor-cpu = Processor
desk-monitor-memory = Memory
desk-monitor-temperature = Temperature
desk-monitor-cpu-short = CPU
desk-monitor-memory-short = RAM


## "Join a workspace…": the directory of public workspaces.
directory-title = Join a workspace
directory-search = Search public workspaces, or paste a link or code
directory-link-or-code = Join with a link or code
directory-close = Close
directory-loading = Looking for public workspaces…
directory-empty = No public workspaces here yet. Type a site like tilo.coffee and press Enter to see its workspaces.
directory-no-match = Nothing matches “{ $query }”. Press Enter to look it up as a site.
directory-failed = The directory could not be read: { $error }
directory-join-failed = Couldn't join { $name }: { $error }
directory-join = Join
directory-open = Open
directory-joining = Joining…
directory-join-named = Join { $name }
directory-open-named = Open { $name }
directory-community-tag = { $host }'s own
directory-try-again = Try again
directory-already-joining = You're already joining { $name }. It opens as soon as it's ready.
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
directory-reported = Workspace reported
directory-reported-body = The people who run it have been told.
directory-report-failed = The report didn't go through
directory-moved = { $name } moved to your host, so anyone can find and join it.
arrival-others = { $count ->
    [one] and 1 other
   *[other] and { $count } others
}

# Cursor chat: a short message beside your pointer, shown to everyone in that window.
cursor-chat-placeholder = Say something
cursor-chat-you = You

# The line under the top bar when you arrive where people are, or someone waves.
presence-arrival-intro-where = People here see your name, your colour and where you are in this workspace
presence-arrival-intro-here = People here see your name and your colour, never where you are
presence-arrival-intro-nothing = People here don't see you, and you don't see their pointers
presence-arrival-change = Change
profile-name-not-saved = Your name wasn’t saved
profile-action-say-something = Say something here
cursor-chat-no-window = Point at a shared window to say something
cursor-chat-no-window-body = What you say shows beside your pointer to the people in that window
people-join-call-failed = Couldn’t join the call
people-join-call-not-taken = Join it from Chat
presence-wave-notice = { $person } waves
presence-someone = Someone
